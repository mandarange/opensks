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
