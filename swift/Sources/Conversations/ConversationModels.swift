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

/// The `{"ok":true}` style acknowledgement returned by mutating verbs.
struct ConversationAck: Codable, Sendable, Equatable {
    let ok: Bool
}

// MARK: - Run state (PR-027)

/// The final state of a deterministic engine run as surfaced by the
/// conversation-turn / run-list contracts. Lenient string enum with an
/// `.unknown` fallback so a future server value never crashes the decoder.
enum RunState: String, Codable, Sendable, Equatable {
    case queued
    case running
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
        case .cancelled: return .warning
        case .unknown: return .neutral
        }
    }

    var displayLabel: String {
        switch self {
        case .queued: return "Queued"
        case .running: return "Running"
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
/// deterministic engine run that produced the assistant content, and the run's
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
