// Models.swift — Codable structs matching `opensks-cli app-data` JSON exactly
// (decoded with .convertFromSnakeCase), plus pure presentation enums and the
// single honesty gate for completion language.

import SwiftUI

struct AppData: Codable, Sendable {
    let schema: String
    let workspace: String
    let workspaceLabel: String
    let appBundle: String
    let artifactDir: String
    let dashboardHtml: String
    let missionsDir: String
    let cliPath: String
    let acceptance: Acceptance
    let release: ReleaseProofSummary?
    let providerAdapterCheck: ProviderAdapterCheckReport?
    let providerMockE2E: ProviderMockE2eSummary?
    let gui: Gui
    let workerLanes: [WorkerLane]
}

struct Acceptance: Codable, Sendable {
    let total: Int
    let passed: Int
    let partial: Int
    let failed: Int
    let goalComplete: Bool?

    var ratio: Double { total == 0 ? 0 : min(1, Double(passed) / Double(total)) }
}

struct ReleaseProofSummary: Codable, Sendable {
    let status: String
    let blockers: [ReleaseProofBlocker]
    let remediationActions: [ReleaseRemediationAction]

    var hasEvidence: Bool {
        status != "not_audited" || !blockers.isEmpty || !remediationActions.isEmpty
    }

    var displayStatus: String {
        status
            .split(separator: "_")
            .map { part in part.prefix(1).uppercased() + String(part.dropFirst()) }
            .joined(separator: " ")
    }

    var pillKind: StatusPill.Kind {
        if status == "verified" || status == "passed" { return .success }
        if status == "invalid" || status == "failed" { return .danger }
        if !blockers.isEmpty || status == "not_verified" { return .warning }
        return .neutral
    }
}

struct ReleaseProofBlocker: Codable, Sendable, Identifiable {
    let code: String
    let message: String

    var id: String { code }
}

struct ReleaseRemediationAction: Codable, Sendable, Identifiable {
    let blocker: String
    let action: String
    let scope: String

    var id: String { "\(scope):\(blocker):\(action)" }
}

struct ProviderMockE2eSummary: Codable, Sendable {
    let status: String
    let fixtureKind: String
    let liveVendorCallsPerformed: Bool
    let secretValueExposed: Bool
    let modelCatalogCount: Int
    let modelCatalogSynced: Bool
    let modelEnabled: Bool
    let registryRouteStatus: String
    let selectedModelId: String?
    let checks: [ProviderMockE2eCheck]

    var hasEvidence: Bool {
        status != "not_audited" || !checks.isEmpty
    }

    var displayStatus: String {
        status
            .split(separator: "_")
            .map { part in part.prefix(1).uppercased() + String(part.dropFirst()) }
            .joined(separator: " ")
    }

    var pillKind: StatusPill.Kind {
        if status == "verified" || status == "passed" { return .success }
        if status == "invalid" || status == "failed" { return .danger }
        if status == "partial" || status == "not_verified" { return .warning }
        return .neutral
    }
}

struct ProviderMockE2eCheck: Codable, Sendable, Identifiable {
    let id: String
    let status: String
    let evidenceRef: String
}

struct Gui: Codable, Sendable {
    let prdTotal: Int
    let prdImplemented: Int
    let prdArtifactMvp: Int
    let prdPlanned: Int
    let prdMissingLive: Int
    let qaStatus: String
    let securityStatus: String
    let providerConfiguredCount: Int
    let voxelCount: Int
    let missionCount: Int
    let browserSessions: Int
    let computerSessions: Int
    let appSessions: Int
    let workerLaneMissions: Int
    let workerLaneCount: Int
}

struct WorkerLane: Codable, Sendable, Identifiable {
    let missionId: String
    let status: String
    let executionMode: String
    let laneCount: Int
    let workerLanes: [String]
    let source: String

    var id: String { missionId }
}

protocol OpenSKSStringEnum: Codable, Sendable, Equatable, CustomStringConvertible {
    var rawValue: String { get }
    init(rawValue: String)
}

extension OpenSKSStringEnum {
    var description: String { rawValue }

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        self.init(rawValue: try container.decode(String.self))
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

enum EngineEventType: OpenSKSStringEnum {
    case engineHello
    case engineHealth
    case executionEvent
    case error
    /// STREAM-001: the explicit per-request terminal marker. The daemon emits one
    /// as the final event of every request response; the client completes on it
    /// rather than on a silence/quiet-window heuristic.
    case requestCompleted
    case unrecognized(String)

    var rawValue: String {
        switch self {
        case .engineHello: return "engine_hello"
        case .engineHealth: return "engine_health"
        case .executionEvent: return "execution_event"
        case .error: return "error"
        case .requestCompleted: return "request_completed"
        case .unrecognized(let value): return value
        }
    }

    init(rawValue: String) {
        switch rawValue {
        case "engine_hello": self = .engineHello
        case "engine_health": self = .engineHealth
        case "execution_event": self = .executionEvent
        case "error": self = .error
        case "request_completed": self = .requestCompleted
        default: self = .unrecognized(rawValue)
        }
    }
}

enum EngineEventSeverity: OpenSKSStringEnum {
    case info
    case warning
    case error
    case unrecognized(String)

    var rawValue: String {
        switch self {
        case .info: return "info"
        case .warning: return "warning"
        case .error: return "error"
        case .unrecognized(let value): return value
        }
    }

    init(rawValue: String) {
        switch rawValue {
        case "info": self = .info
        case "warning": self = .warning
        case "error": self = .error
        default: self = .unrecognized(rawValue)
        }
    }

    var isError: Bool { self == .error }
}

enum ExecutionEventKind: OpenSKSStringEnum {
    case runStarted
    case runPaused
    case runResumed
    case runCancelled
    case steeringRequested
    case approvalRequested
    case approvalApproved
    case approvalDenied
    case workItemQueued
    case workItemLeased
    case workItemRunning
    case workItemCompleted
    case leaseHeartbeat
    case leaseExpired
    case verificationPassed
    case verificationFailed
    case gitCommitReceipt
    case gitPushReceipt
    case gitPushFailed
    case imageArtifactCreated
    case snapshotWritten
    case queueActionRequested
    case unknown
    case unrecognized(String)

    var rawValue: String {
        switch self {
        case .runStarted: return "run_started"
        case .runPaused: return "run_paused"
        case .runResumed: return "run_resumed"
        case .runCancelled: return "run_cancelled"
        case .steeringRequested: return "steering_requested"
        case .approvalRequested: return "approval_requested"
        case .approvalApproved: return "approval_approved"
        case .approvalDenied: return "approval_denied"
        case .workItemQueued: return "work_item_queued"
        case .workItemLeased: return "work_item_leased"
        case .workItemRunning: return "work_item_running"
        case .workItemCompleted: return "work_item_completed"
        case .leaseHeartbeat: return "lease_heartbeat"
        case .leaseExpired: return "lease_expired"
        case .verificationPassed: return "verification_passed"
        case .verificationFailed: return "verification_failed"
        case .gitCommitReceipt: return "git_commit_receipt"
        case .gitPushReceipt: return "git_push_receipt"
        case .gitPushFailed: return "git_push_failed"
        case .imageArtifactCreated: return "image_artifact_created"
        case .snapshotWritten: return "snapshot_written"
        case .queueActionRequested: return "queue_action_requested"
        case .unknown: return "unknown"
        case .unrecognized(let value): return value
        }
    }

    init(rawValue: String) {
        switch rawValue {
        case "run_started": self = .runStarted
        case "run_paused": self = .runPaused
        case "run_resumed": self = .runResumed
        case "run_cancelled": self = .runCancelled
        case "steering_requested": self = .steeringRequested
        case "approval_requested": self = .approvalRequested
        case "approval_approved": self = .approvalApproved
        case "approval_denied": self = .approvalDenied
        case "work_item_queued": self = .workItemQueued
        case "work_item_leased": self = .workItemLeased
        case "work_item_running": self = .workItemRunning
        case "work_item_completed": self = .workItemCompleted
        case "lease_heartbeat": self = .leaseHeartbeat
        case "lease_expired": self = .leaseExpired
        case "verification_passed": self = .verificationPassed
        case "verification_failed": self = .verificationFailed
        case "git_commit_receipt": self = .gitCommitReceipt
        case "git_push_receipt": self = .gitPushReceipt
        case "git_push_failed": self = .gitPushFailed
        case "image_artifact_created": self = .imageArtifactCreated
        case "snapshot_written": self = .snapshotWritten
        case "queue_action_requested": self = .queueActionRequested
        case "unknown": self = .unknown
        default: self = .unrecognized(rawValue)
        }
    }
}

enum EventSensitivity: OpenSKSStringEnum {
    case `public`
    case `internal`
    case secret
    case unrecognized(String)

    var rawValue: String {
        switch self {
        case .public: return "public"
        case .internal: return "internal"
        case .secret: return "secret"
        case .unrecognized(let value): return value
        }
    }

    init(rawValue: String) {
        switch rawValue {
        case "public": self = .public
        case "internal": self = .internal
        case "secret": self = .secret
        default: self = .unrecognized(rawValue)
        }
    }
}

struct EngineEvent: Codable, Sendable, Identifiable {
    let schema: String
    let eventId: String
    let requestId: String?
    let eventType: EngineEventType
    let severity: EngineEventSeverity
    let message: String
    let protocolVersion: String
    let timestampMs: UInt64
    let evidenceRefs: [String]
    let redacted: Bool

    var id: String { eventId }
}

struct ExecutionEventEnvelope: Codable, Sendable, Identifiable {
    let schema: String
    let id: String
    let runId: String
    let sequence: UInt64
    let occurredAt: String
    let actor: String
    let causationId: String?
    let correlationId: String?
    let kind: ExecutionEventKind
    let payload: JSONValue
    let sensitivity: EventSensitivity
    let evidenceRefs: [String]
}

enum JSONValue: Codable, Sendable, Equatable {
    case string(String)
    case number(Double)
    case bool(Bool)
    case object([String: JSONValue])
    case array([JSONValue])
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([String: JSONValue].self) {
            self = .object(value)
        } else {
            self = .array(try container.decode([JSONValue].self))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }

    var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }

    subscript(key: String) -> JSONValue? {
        if case .object(let values) = self { return values[key] }
        return nil
    }
}

struct RunRecord: Identifiable, Equatable, Sendable {
    let id: String
    var state: String
    var lastSequence: UInt64
    var lastMessage: String
    var evidenceRefs: [String]
}

struct QueueItemRecord: Identifiable, Equatable, Sendable {
    let id: String
    let runId: String
    var state: String
    var priority: Int
    var lastSequence: UInt64
}

struct ApprovalRecord: Identifiable, Equatable, Sendable {
    let id: String
    let runId: String
    var scope: String
    var state: String
    var lastSequence: UInt64
}

struct SteeringRecord: Identifiable, Equatable, Sendable {
    let id: String
    let runId: String
    var message: String
    var targetId: String?
    var lastSequence: UInt64
}

// MARK: - Presentation enums

enum RunMode: String, CaseIterable, Identifiable {
    case goal, direct, naruto
    var id: String { rawValue }

    /// CLI verb this mode maps to 1:1.
    var verb: String {
        switch self {
        case .goal: return "goal"
        case .direct: return "run"
        case .naruto: return "naruto"
        }
    }
    /// User-facing name (recovery directive §5.4). The internal `verb` keeps the
    /// legacy CLI mapping; only the product label changes.
    var label: String {
        switch self {
        case .goal: return "Plan & Execute"
        case .direct: return "Quick Edit"
        case .naruto: return "Parallel Build"
        }
    }
    var caption: String {
        switch self {
        case .goal: return "Plan the change, then execute with proof artifacts."
        case .direct: return "Make the change directly in a single pass."
        case .naruto: return "Run several workers in parallel across lanes."
        }
    }
}

enum RailSection: String, CaseIterable, Identifiable {
    case home, graph, runs, queue, models
    case intelligence, git, evidence, files, settings
    var id: String { rawValue }

    var label: String {
        switch self {
        case .home: return "Home"
        case .graph: return "Graph"
        case .runs: return "Runs"
        case .queue: return "Queue"
        case .models: return "Models"
        case .intelligence: return "Project Intelligence"
        case .git: return "Git"
        case .evidence: return "Evidence"
        case .files: return "Files"
        case .settings: return "Settings"
        }
    }
    var symbol: String {
        switch self {
        case .home: return "house"
        case .graph: return "point.3.connected.trianglepath.dotted"
        case .runs: return "sparkles"
        case .queue: return "list.bullet.indent"
        case .models: return "cpu"
        case .intelligence: return "brain.head.profile"
        case .git: return "arrow.triangle.branch"
        case .evidence: return "checkmark.seal"
        case .files: return "folder"
        case .settings: return "gearshape"
        }
    }
}

enum TerminalTab: String, CaseIterable, Identifiable {
    case output, problems, activity
    var id: String { rawValue }
    var label: String { rawValue.prefix(1).uppercased() + rawValue.dropFirst() }
}

/// Maps a worker-lane status string to a dot color + SF Symbol.
enum LaneStatus {
    static func color(_ status: String) -> Color {
        let s = status.lowercased()
        if s == "completed" || s.contains("verified") || s.contains("sealed") { return Theme.accent }
        if s.contains("run") || s.contains("active") { return Theme.blue }
        if s.contains("queue") || s.contains("pending") { return Theme.gold }
        return Theme.muted
    }
    static func symbol(_ status: String) -> String {
        let s = status.lowercased()
        if s == "completed" || s.contains("verified") || s.contains("sealed") { return "checkmark.circle.fill" }
        if s.contains("run") || s.contains("active") { return "circle.dotted" }
        if s.contains("queue") || s.contains("pending") { return "circle" }
        return "circle.dashed"
    }
}

/// The ONLY source of completion language. It never returns "complete".
enum HonestText {
    static func goalState(_ a: Acceptance) -> String {
        switch a.goalComplete {
        case .some(true): return "Verifying"
        case .some(false): return "In progress"
        case .none: return "Unknown"
        }
    }
    static func acceptanceLine(_ a: Acceptance) -> String {
        "\(a.passed) passed · \(a.partial) partial · \(a.failed) failed"
    }
    static func statusLine(_ a: Acceptance) -> String {
        "\(a.passed) passed / \(a.partial) partial / \(a.failed) failed · \(goalState(a))"
    }
}

func isPass(_ status: String) -> Bool {
    let s = status.lowercased()
    return s == "pass" || s == "passed" || s == "ok"
}
