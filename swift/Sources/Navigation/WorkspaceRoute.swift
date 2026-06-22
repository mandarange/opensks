// WorkspaceRoute.swift — typed destinations for the conversation-first shell.
//
// PR-022: a flat route set drives the *central* workspace, not just the sidebar.
// Associated-value routes (conversation / editor / design identities) arrive
// with their domain types in later PRs; this bootstrap keeps the routing typed
// and the rail labelled.

import Foundation

enum WorkspaceRoute: String, CaseIterable, Hashable, Identifiable, Codable {
    case home, chat, code, graph, runs, git, design, intelligence, vault, evidence, settings, project

    var id: String { rawValue }

    /// The five PRIMARY rail destinations (recovery directive §3.4). The rest
    /// (Intelligence, Design, Evidence, Vault, Settings, Runs, Home) are reached
    /// through the Project hub, keeping the rail uncluttered.
    static let primaryRailRoutes: [WorkspaceRoute] = [.chat, .code, .git, .graph, .project]

    /// Visible rail label (English).
    var label: String {
        switch self {
        case .home: return "Home"
        case .chat: return "Chat"
        case .code: return "Code"
        case .graph: return "Pipeline"
        case .runs: return "Runs"
        case .git: return "Changes"
        case .design: return "Design"
        case .intelligence: return "Intel"
        case .vault: return "Vault"
        case .evidence: return "Evidence"
        case .settings: return "Settings"
        case .project: return "Project"
        }
    }

    var symbol: String {
        switch self {
        case .home: return "house"
        case .chat: return "bubble.left.and.bubble.right"
        case .code: return "chevron.left.forwardslash.chevron.right"
        case .graph: return "point.3.connected.trianglepath.dotted"
        case .runs: return "sparkles"
        case .git: return "arrow.triangle.branch"
        case .design: return "paintpalette"
        case .intelligence: return "brain.head.profile"
        case .vault: return "lock.shield"
        case .evidence: return "checkmark.seal"
        case .settings: return "gearshape"
        case .project: return "square.grid.2x2"
        }
    }

    /// Stable identifier for the central surface of this route (UI/accessibility).
    var centralAccessibilityIdentifier: String { "workspace.central.\(rawValue)" }

    /// Stable identifier for this route's rail tile (UI/accessibility).
    var railTileAccessibilityIdentifier: String { "rail.tile.\(rawValue)" }

    /// Legacy context-sidebar section reused while per-route sidebars land in
    /// later PRs (PR-025 conversations, PR-040 design, etc.). Total mapping.
    var legacySection: RailSection {
        switch self {
        case .home: return .home
        case .chat: return .home
        case .code: return .files
        case .graph: return .graph
        case .runs: return .runs
        case .git: return .git
        case .design: return .home
        case .intelligence: return .intelligence
        case .vault: return .home
        case .evidence: return .evidence
        case .settings: return .settings
        case .project: return .home
        }
    }
}
