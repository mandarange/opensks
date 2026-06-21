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
    var label: String {
        switch self {
        case .goal: return "Goal"
        case .direct: return "Direct"
        case .naruto: return "Naruto"
        }
    }
    var caption: String {
        switch self {
        case .goal: return "Bounded goal-loop with stop policy and proof artifacts."
        case .direct: return "Immediate single-pass execution."
        case .naruto: return "Multi-wave agent coordination across lanes."
        }
    }
}

enum RailSection: String, CaseIterable, Identifiable {
    case explorer, agentRuns, providers, proof, artifacts
    var id: String { rawValue }

    var label: String {
        switch self {
        case .explorer: return "Explorer"
        case .agentRuns: return "Agent runs"
        case .providers: return "Providers"
        case .proof: return "Proof"
        case .artifacts: return "Artifacts"
        }
    }
    var symbol: String {
        switch self {
        case .explorer: return "folder"
        case .agentRuns: return "sparkles"
        case .providers: return "powerplug"
        case .proof: return "checkmark.seal"
        case .artifacts: return "square.stack.3d.up"
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
        if s.contains("complete") || s.contains("done") || s.contains("sealed") { return Theme.accent }
        if s.contains("run") || s.contains("active") { return Theme.blue }
        if s.contains("queue") || s.contains("pending") { return Theme.gold }
        return Theme.muted
    }
    static func symbol(_ status: String) -> String {
        let s = status.lowercased()
        if s.contains("complete") || s.contains("done") || s.contains("sealed") { return "checkmark.circle.fill" }
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
