// ConversationModels.swift — Codable mirrors of the PR-025 conversation wire
// contract (snake_case JSON, decoded with .convertFromSnakeCase exactly like the
// rest of the app). Status / role / state are string enums with an `.unknown`
// fallback so a future server value never crashes the decoder.

import Foundation

// MARK: - String enums (snake_case, lenient)

enum ConversationStatus: String, Codable, Sendable, Equatable, CaseIterable {
    case idle
    case running
    case paused
    case completed
    case failed
    case archived
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ConversationStatus(rawValue: raw) ?? .unknown
    }

    /// Status as surfaced by a `StatusPill` (glyph + tint, never colour alone).
    var pillKind: StatusPill.Kind {
        switch self {
        case .running: return .running
        case .completed: return .success
        case .failed: return .danger
        case .paused, .archived: return .warning
        case .idle, .unknown: return .neutral
        }
    }

    var displayLabel: String {
        switch self {
        case .idle: return "Idle"
        case .running: return "Running"
        case .paused: return "Paused"
        case .completed: return "Done"
        case .failed: return "Failed"
        case .archived: return "Archived"
        case .unknown: return "Unknown"
        }
    }
}

enum MessageRole: String, Codable, Sendable, Equatable {
    case user
    case assistant
    case system
    case tool
    case event
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = MessageRole(rawValue: raw) ?? .unknown
    }
}

enum MessageState: String, Codable, Sendable, Equatable {
    case pending
    case streaming
    case complete
    case failed
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = MessageState(rawValue: raw) ?? .unknown
    }
}

// MARK: - Summary

/// Mirrors `opensks_contracts::ConversationSummary`.
struct ConversationSummary: Codable, Sendable, Identifiable, Equatable {
    let schema: String
    let id: String
    let projectId: String
    let title: String
    let titleSource: String
    let status: ConversationStatus
    let pinned: Bool
    let archived: Bool
    let messageCount: Int
    let createdAtMs: Int64
    let updatedAtMs: Int64
    let lastMessageAtMs: Int64?

    /// Most recent activity timestamp for ordering / relative-time display.
    var activityMs: Int64 { lastMessageAtMs ?? updatedAtMs }

    var lastActivityDate: Date {
        Date(timeIntervalSince1970: Double(activityMs) / 1000.0)
    }
}

// MARK: - Message

/// Mirrors `opensks_contracts::ConversationMessage`.
struct ConversationMessage: Codable, Sendable, Identifiable, Equatable {
    let schema: String
    let id: String
    let projectId: String
    let conversationId: String
    let turnId: String?
    let role: MessageRole
    let state: MessageState
    let contentRedacted: String
    let sequence: Int64
    let createdAtMs: Int64
    let updatedAtMs: Int64

    var createdAtDate: Date {
        Date(timeIntervalSince1970: Double(createdAtMs) / 1000.0)
    }
}

// MARK: - Envelopes

/// Mirrors the `conversation list` envelope.
struct ConversationList: Codable, Sendable, Equatable {
    let schema: String
    let projectId: String
    let conversations: [ConversationSummary]
}

/// Mirrors the `conversation messages` envelope.
struct MessagePage: Codable, Sendable, Equatable {
    let conversationId: String
    let messages: [ConversationMessage]
    let hasMore: Bool
}

// MARK: - Timeline

/// Mirrors `opensks_contracts::TimelineItemKind` for the durable conversation
/// timeline projection. Unknown future kinds decode safely instead of crashing
/// the Chat surface.
enum ConversationTimelineItemKind: String, Codable, Sendable, Equatable {
    case userMessage = "user_message"
    case assistantMessage = "assistant_message"
    case plan
    case toolCall = "tool_call"
    case worker
    case patch
    case verification
    case approval
    case commitReceipt = "commit_receipt"
    case pushReceipt = "push_receipt"
    case imageArtifact = "image_artifact"
    case warning
    case error
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ConversationTimelineItemKind(rawValue: raw) ?? .unknown
    }

    var displayLabel: String {
        switch self {
        case .userMessage: return "You"
        case .assistantMessage: return "Assistant"
        case .plan: return "Plan"
        case .toolCall: return "Tool"
        case .worker: return "Worker"
        case .patch: return "Patch"
        case .verification: return "Verification"
        case .approval: return "Approval"
        case .commitReceipt: return "Commit"
        case .pushReceipt: return "Push"
        case .imageArtifact: return "Image"
        case .warning: return "Warning"
        case .error: return "Error"
        case .unknown: return "Timeline"
        }
    }
}

/// Secret-redacted payload for the current message-backed timeline projection.
/// Future event kinds may carry different shapes, so every field is optional.
struct ConversationTimelinePayload: Codable, Sendable, Equatable {
    let messageId: String?
    let role: MessageRole?
    let messageState: MessageState?
    let contentRedacted: String?
    let runRelation: String?
    let commit: String?
    let paths: [String]?
    let message: String?
    let remote: String?
    let ref: String?
    let remoteOid: String?
    let localOid: String?
    let alreadyDone: Bool?
    let sourceSchema: String?
    let projection: String?
    let committed: Bool?
    let pushed: Bool?
    let intentId: String?
    let effectDigest: String?
    let idempotencyKey: String?
    let remoteUrlRedacted: String?
    let remoteExpectedOid: String?
    let protected: Bool?
    let approvalId: String?
    let approvalMatched: Bool?
}

/// Mirrors one `opensks.timeline-item.v1` entry from
/// `opensks conversation timeline`.
struct ConversationTimelineItem: Codable, Sendable, Identifiable, Equatable {
    let schema: String
    let id: String
    let projectId: String
    let conversationId: String
    let turnId: String?
    let runId: String?
    let sequence: Int64
    let kind: ConversationTimelineItemKind
    let state: String
    let payload: ConversationTimelinePayload
    let createdAtMs: Int64
    let updatedAtMs: Int64

    var createdAtDate: Date {
        Date(timeIntervalSince1970: Double(createdAtMs) / 1000.0)
    }

    var message: ConversationMessage? {
        guard let messageId = payload.messageId,
              let role = payload.role,
              let messageState = payload.messageState
        else { return nil }
        return ConversationMessage(
            schema: "opensks.conversation-message.v1",
            id: messageId,
            projectId: projectId,
            conversationId: conversationId,
            turnId: turnId,
            role: role,
            state: messageState,
            contentRedacted: payload.contentRedacted ?? "",
            sequence: sequence,
            createdAtMs: createdAtMs,
            updatedAtMs: updatedAtMs
        )
    }

    var commitCard: GitCommitCard? {
        guard kind == .commitReceipt, let commit = payload.commit else { return nil }
        return GitCommitCard(
            id: id,
            commit: commit,
            paths: payload.paths ?? [],
            message: payload.message ?? "",
            committedAtMs: createdAtMs
        )
    }

    var pushCard: GitPushCard? {
        guard kind == .pushReceipt,
              let remote = payload.remote,
              let ref = payload.ref,
              let remoteOid = payload.remoteOid,
              let localOid = payload.localOid
        else { return nil }
        return GitPushCard(
            id: id,
            remote: remote,
            ref: ref,
            remoteOid: remoteOid,
            localOid: localOid,
            alreadyDone: payload.alreadyDone ?? false,
            pushedAtMs: createdAtMs
        )
    }
}

/// Mirrors the `opensks.conversation-timeline.v1` envelope.
struct ConversationTimeline: Codable, Sendable, Equatable {
    let schema: String
    let conversationId: String
    let items: [ConversationTimelineItem]
}

/// The `{"ok":true}` style acknowledgement returned by mutating verbs.
struct ConversationAck: Codable, Sendable, Equatable {
    let ok: Bool
}

// MARK: - Run state (PR-027)

/// The final state of a conversation runtime run as surfaced by the
/// conversation-turn / run-list contracts. Lenient string enum with an
/// `.unknown` fallback so a future server value never crashes the decoder.
enum RunState: String, Codable, Sendable, Equatable {
    case queued
    case running
    case paused
    case completed
    case failed
    case cancelled
    case unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = RunState(rawValue: raw) ?? .unknown
    }

    /// Run state as surfaced by a `StatusPill` (glyph + tint, never colour alone).
    var pillKind: StatusPill.Kind {
        switch self {
        case .running, .queued: return .running
        case .completed: return .success
        case .failed: return .danger
        case .paused, .cancelled: return .warning
        case .unknown: return .neutral
        }
    }

    var displayLabel: String {
        switch self {
        case .queued: return "Queued"
        case .running: return "Running"
        case .paused: return "Paused"
        case .completed: return "Done"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        case .unknown: return "Unknown"
        }
    }
}

// MARK: - Turn (PR-027)

/// Mirrors `opensks.conversation-turn.v1` — the result of starting ONE turn:
/// the persisted user message, the assistant placeholder message it links, the
/// runtime run that produced the assistant content, and the run's
/// final state. `reused` is true when the turn was de-duplicated against a
/// previously-seen idempotency key (no second run was started).
struct ConversationTurn: Codable, Sendable, Equatable {
    let schema: String
    let turnId: String
    let userMessageId: String
    let assistantMessageId: String
    let runId: String
    let runState: RunState
    let reused: Bool
}

// MARK: - Turn start v2 (daemon accepted-handle path)

enum ModelSelectionMode: String, Codable, Sendable, Equatable, CaseIterable {
    case auto
    case pinned

    var displayLabel: String {
        switch self {
        case .auto: return "Auto"
        case .pinned: return "Pinned"
        }
    }
}

struct ModelSelection: Codable, Sendable, Equatable {
    var mode: ModelSelectionMode
    var modelId: String?
    var fallbackModelIds: [String]
}

enum ReasoningEffort: String, Codable, Sendable, Equatable, CaseIterable {
    case quick
    case standard
    case deep
    case maximum

    var displayLabel: String {
        switch self {
        case .quick: return "Quick"
        case .standard: return "Standard"
        case .deep: return "Deep"
        case .maximum: return "Maximum"
        }
    }
}

enum ExecutionMode: String, Codable, Sendable, Equatable, CaseIterable {
    case local
    case worktree
    case readOnly = "read_only"
    case cloud

    var displayLabel: String {
        switch self {
        case .local: return "Local"
        case .worktree: return "Worktree"
        case .readOnly: return "Read-only"
        case .cloud: return "Cloud"
        }
    }
}

/// Mirrors `opensks_contracts::ConversationTurnSettings`.
struct ConversationTurnSettings: Codable, Sendable, Equatable {
    let model: ModelSelection
    let reasoningEffort: ReasoningEffort
    let executionMode: ExecutionMode
    let pipelineId: String
    let graphRevision: String?
    let maxParallelism: UInt32
    let verifierCount: UInt32
    let toolPolicyId: String
    let approvalPolicyId: String
    let tokenBudget: UInt64?
    let costBudgetUsd: Double?
    let timeoutMs: UInt64?
    let imageModelId: String?

    static func defaultForTurn() -> ConversationTurnSettings {
        ConversationTurnSettings(
            model: ModelSelection(mode: .auto, modelId: nil, fallbackModelIds: []),
            reasoningEffort: .standard,
            executionMode: .worktree,
            pipelineId: "auto",
            graphRevision: nil,
            maxParallelism: 4,
            verifierCount: 1,
            toolPolicyId: "project-default",
            approvalPolicyId: "safe-interactive",
            tokenBudget: nil,
            costBudgetUsd: nil,
            timeoutMs: nil,
            imageModelId: nil
        )
    }
}

/// Mirrors `opensks_contracts::ConversationThreadSettings`: durable per-thread
/// Chat settings persisted by the conversation repository. It intentionally
/// carries only ids/refs, never secret values.
struct ConversationThreadSettings: Codable, Sendable, Equatable {
    let schema: String
    var conversationId: String
    var modelSelection: ModelSelection
    var reasoningEffort: ReasoningEffort
    var executionMode: ExecutionMode
    var pipelineId: String
    var maxParallelism: UInt32
    var verifierCount: UInt32
    var toolPolicyId: String
    var approvalPolicyId: String
    var imageModelId: String?
    var updatedAtMs: Int64

    static func defaultFor(conversationID: String, updatedAtMs: Int64 = 0) -> ConversationThreadSettings {
        ConversationThreadSettings(
            schema: "opensks.thread-settings.v1",
            conversationId: conversationID,
            modelSelection: ModelSelection(mode: .auto, modelId: nil, fallbackModelIds: []),
            reasoningEffort: .standard,
            executionMode: .worktree,
            pipelineId: "auto",
            maxParallelism: 4,
            verifierCount: 1,
            toolPolicyId: "project-default",
            approvalPolicyId: "safe-interactive",
            imageModelId: nil,
            updatedAtMs: updatedAtMs
        )
    }
}

struct UserMessageInput: Codable, Sendable, Equatable {
    let text: String
    let attachmentRefs: [String]
}

struct TurnContextSelection: Codable, Sendable, Equatable {
    let refs: [String]

    static let empty = TurnContextSelection(refs: [])
}

/// Submitted by Swift Chat to the daemon. The daemon persists the accepted turn
/// and returns `ConversationTurnAccepted` without waiting for adapter execution.
struct ConversationTurnStartRequest: Codable, Sendable, Equatable {
    let schema: String
    let requestId: String
    let projectId: String
    let conversationId: String
    let clientTurnId: String
    let message: UserMessageInput
    let settings: ConversationTurnSettings
    let context: TurnContextSelection
    let idempotencyKey: String
}

/// Mirrors `opensks.conversation-turn-accepted.v1`.
struct ConversationTurnAccepted: Codable, Sendable, Equatable {
    let schema: String
    let requestId: String
    let turnId: String
    let runId: String
    let userMessageId: String
    let assistantMessageId: String
    let streamId: String
    let state: RunState
}

// MARK: - Turn supervisor tick (daemon execution path)

/// Mirrors `opensks.turn-supervisor-tick.v1`, emitted by the daemon after a
/// supervisor recovers expired leases, claims at most one queued accepted turn,
/// and executes it through the adapter/runtime path.
struct TurnSupervisorTickResult: Codable, Sendable, Equatable {
    let schema: String
    let requestId: String
    let supervisorId: String
    let recoveredExpiredLeases: UInt64
    let claimed: TurnSupervisorClaimedTurn?
    let executed: TurnSupervisorExecution?
}

struct TurnSupervisorClaimedTurn: Codable, Sendable, Equatable {
    let turnId: String
    let runId: String
    let projectId: String
    let conversationId: String
    let assistantMessageId: String
    let leaseOwner: String
    let leaseExpiresAtMs: UInt64
    let hasModelRoutingDecision: Bool
}

struct TurnSupervisorExecution: Codable, Sendable, Equatable {
    let status: String
    let runState: RunState
    let assistantMessageId: String?
    let lastEventSequence: UInt64
    let patchCount: Int?
    let applyResultCount: Int?
    let error: String?
}

/// Mirrors one entry of `opensks.conversation-run-list.v1`: a run linked to a
/// conversation turn/message with its relation ("primary") and final state.
struct ConversationRunRef: Codable, Sendable, Identifiable, Equatable {
    let turnId: String
    let runId: String
    let messageId: String
    let relation: String
    let runState: RunState

    /// Stable identity for `ForEach` — a run is unique within a conversation.
    var id: String { runId }
}

/// Mirrors the `opensks.conversation-run-list.v1` envelope.
struct ConversationRunList: Codable, Sendable, Equatable {
    let schema: String
    let conversationId: String
    let runs: [ConversationRunRef]
}

// MARK: - Filters

/// Sidebar filter; raw value matches the CLI `--filter` argument.
enum ConversationFilter: String, CaseIterable, Sendable, Identifiable {
    case all
    case running
    case pinned
    case archived

    var id: String { rawValue }

    var label: String {
        switch self {
        case .all: return "All"
        case .running: return "Running"
        case .pinned: return "Pinned"
        case .archived: return "Archived"
        }
    }
}
