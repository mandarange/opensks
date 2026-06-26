// Capability.swift — the Swift view of the runtime capability registry
// (recovery directive §18). The app shows each capability's honest maturity
// ("Available" / "Needs setup" / "Simulation" / "Unavailable") read from the
// product CLI `opensks capability report`, so the UI never presents a
// foundation/simulation surface as if it were live.

import Foundation
import SwiftUI

/// How real a capability is at runtime. Decoded from the snake_case the Rust
/// `CapabilityMaturity` emits.
enum CapabilityMaturity: String, Codable, Sendable {
    case live
    case degraded
    case foundation
    case simulation
    case unavailable

    /// The user-facing label (matches opensks_contracts §18.3).
    var displayLabel: String {
        switch self {
        case .live: return "Available"
        case .degraded: return "Limited"
        case .foundation: return "Needs setup"
        case .simulation: return "Simulation"
        case .unavailable: return "Unavailable"
        }
    }

    /// A tint conveying maturity by colour AND label (never colour alone).
    var tint: Color {
        switch self {
        case .live: return Theme.accent
        case .degraded, .foundation: return Theme.gold
        case .simulation, .unavailable: return Theme.muted
        }
    }
}

struct RuntimeCapability: Codable, Identifiable, Sendable {
    let schema: String
    let id: String
    let title: String
    let maturity: CapabilityMaturity
    let available: Bool
    let reasonCode: String
    let evidenceRefs: [String]
    let actions: [String]

    enum CodingKeys: String, CodingKey {
        case schema, id, title, maturity, available
        case reasonCode = "reason_code"
        case evidenceRefs = "evidence_refs"
        case actions
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(String.self, forKey: .schema)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decode(String.self, forKey: .title)
        maturity = try c.decode(CapabilityMaturity.self, forKey: .maturity)
        available = try c.decode(Bool.self, forKey: .available)
        reasonCode = try c.decode(String.self, forKey: .reasonCode)
        // evidence_refs / actions are omitted from the JSON when empty.
        evidenceRefs = (try? c.decode([String].self, forKey: .evidenceRefs)) ?? []
        actions = (try? c.decode([String].self, forKey: .actions)) ?? []
    }
}

enum ToolAvailability: String, Codable, Sendable {
    case available
    case unavailable

    var isAvailable: Bool { self == .available }

    var displayLabel: String {
        switch self {
        case .available: return "Available"
        case .unavailable: return "Disabled"
        }
    }

    var tint: Color {
        switch self {
        case .available: return Theme.accent
        case .unavailable: return Theme.muted
        }
    }
}

enum ToolPermission: String, Codable, Sendable {
    case deny
    case readOnly = "read_only"
    case ask
    case allow

    var displayLabel: String {
        switch self {
        case .deny: return "Deny"
        case .readOnly: return "Read-only"
        case .ask: return "Ask"
        case .allow: return "Allow"
        }
    }
}

struct ToolDescriptor: Codable, Identifiable, Sendable {
    let schema: String
    let name: String
    let displayName: String
    let description: String
    let permission: ToolPermission
    let availability: ToolAvailability
    let reasonCode: String
    let evidenceRefs: [String]

    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case schema, name, description, permission, availability
        case displayName = "display_name"
        case reasonCode = "reason_code"
        case evidenceRefs = "evidence_refs"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(String.self, forKey: .schema)
        name = try c.decode(String.self, forKey: .name)
        displayName = try c.decode(String.self, forKey: .displayName)
        description = try c.decode(String.self, forKey: .description)
        permission = try c.decode(ToolPermission.self, forKey: .permission)
        availability = try c.decode(ToolAvailability.self, forKey: .availability)
        reasonCode = try c.decode(String.self, forKey: .reasonCode)
        evidenceRefs = (try? c.decode([String].self, forKey: .evidenceRefs)) ?? []
    }
}

struct ToolRegistry: Codable, Sendable {
    let schema: String
    let registryId: String
    let revision: UInt64
    let tools: [ToolDescriptor]

    enum CodingKeys: String, CodingKey {
        case schema, revision, tools
        case registryId = "registry_id"
    }

    func descriptor(named name: String) -> ToolDescriptor? {
        tools.first { $0.name == name }
    }

    var availableToolCount: Int {
        tools.filter { $0.availability.isAvailable }.count
    }

    var unavailableToolCount: Int {
        tools.count - availableToolCount
    }

    var sortedTools: [ToolDescriptor] {
        tools.sorted { left, right in
            if left.availability != right.availability {
                return left.availability.isAvailable && !right.availability.isAvailable
            }
            return left.name < right.name
        }
    }
}

struct RuntimeCapabilityReport: Codable, Sendable {
    let schema: String
    let generatedFor: String?
    let capabilities: [RuntimeCapability]
    let toolRegistry: ToolRegistry?

    enum CodingKeys: String, CodingKey {
        case schema
        case generatedFor = "generated_for"
        case capabilities
        case toolRegistry = "tool_registry"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(String.self, forKey: .schema)
        generatedFor = try? c.decode(String.self, forKey: .generatedFor)
        capabilities = try c.decode([RuntimeCapability].self, forKey: .capabilities)
        toolRegistry = try? c.decode(ToolRegistry.self, forKey: .toolRegistry)
    }

    /// Decode a report from the raw `opensks capability report` JSON.
    static func decode(from data: Data) -> RuntimeCapabilityReport? {
        try? JSONDecoder().decode(RuntimeCapabilityReport.self, from: data)
    }

    private static let processSupervisor = ProcessSupervisor()

    /// Run the bundled CLI `capability report` and decode it. Returns `nil` if
    /// the CLI is unavailable or the output cannot be parsed (the caller shows a
    /// truthful "unavailable" state rather than inventing data).
    static func load(cli: URL, workspace: URL) async -> RuntimeCapabilityReport? {
        do {
            let result = try await processSupervisor.run(ProcessSupervisor.Spec(
                executable: cli,
                arguments: ["capability", "report"],
                workingDirectory: OpenSKSCLIProcess.workingDirectory(for: workspace),
                environment: OpenSKSCLIProcess.environmentOverlay(for: workspace),
                timeoutSeconds: OpenSKSCLIProcess.commandTimeoutSeconds,
                maxCaptureBytes: 1024 * 1024
            ))
            guard result.exitCode == 0, !result.timedOut else { return nil }
            return RuntimeCapabilityReport.decode(from: result.stdout)
        } catch {
            return nil
        }
    }
}

/// A compact, accessible list of runtime capabilities with their honest
/// maturity. Read-only; the report is the single source of truth.
struct CapabilityStatusView: View {
    let capabilities: [RuntimeCapability]

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            ForEach(capabilities) { capability in
                HStack(alignment: .firstTextBaseline, spacing: Theme.s10) {
                    Text(capability.title)
                        .font(Theme.ui(12.5))
                        .foregroundStyle(Theme.text)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Text(capability.maturity.displayLabel)
                        .font(Theme.ui(11, .semibold))
                        .foregroundStyle(capability.maturity.tint)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 2)
                        .background(
                            Capsule().fill(capability.maturity.tint.opacity(0.14))
                        )
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("\(capability.title): \(capability.maturity.displayLabel)")
            }
        }
    }
}

/// Read-only view of canonical tool availability. Unavailable tools are rendered
/// as disabled rows, because the registry is the only source that may unlock a
/// tool in product UI.
struct ToolRegistryStatusView: View {
    let registry: ToolRegistry

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.s8) {
            HStack(spacing: Theme.s8) {
                Text("Tool registry")
                    .font(Theme.ui(13, .semibold))
                    .foregroundStyle(Theme.text)
                Text("\(registry.availableToolCount) available")
                    .font(Theme.ui(11, .semibold))
                    .foregroundStyle(Theme.accent)
                if registry.unavailableToolCount > 0 {
                    Text("\(registry.unavailableToolCount) disabled")
                        .font(Theme.ui(11, .semibold))
                        .foregroundStyle(Theme.muted)
                }
                Spacer(minLength: 0)
            }
            ForEach(registry.sortedTools) { tool in
                ToolStatusRow(tool: tool)
            }
        }
        .accessibilityIdentifier("settings.tools.registry")
    }
}

private struct ToolStatusRow: View {
    let tool: ToolDescriptor

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.s10) {
            VStack(alignment: .leading, spacing: 2) {
                Text(tool.displayName)
                    .font(Theme.ui(12.5, .medium))
                    .foregroundStyle(tool.availability.isAvailable ? Theme.text : Theme.muted)
                Text(tool.name)
                    .font(Theme.mono(10.5))
                    .foregroundStyle(Theme.faint)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(tool.permission.displayLabel)
                .font(Theme.ui(11, .semibold))
                .foregroundStyle(Theme.textSoft)
                .frame(width: 68, alignment: .leading)
            Text(tool.availability.displayLabel)
                .font(Theme.ui(11, .semibold))
                .foregroundStyle(tool.availability.tint)
                .padding(.horizontal, 8)
                .padding(.vertical, 2)
                .background(
                    Capsule().fill(tool.availability.tint.opacity(0.14))
                )
        }
        .opacity(tool.availability.isAvailable ? 1 : 0.58)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(
            "\(tool.displayName): \(tool.availability.displayLabel), \(tool.permission.displayLabel), \(tool.reasonCode)"
        )
        .accessibilityIdentifier("settings.tools.\(tool.name)")
    }
}
