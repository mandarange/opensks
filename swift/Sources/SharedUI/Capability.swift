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

struct RuntimeCapabilityReport: Codable, Sendable {
    let schema: String
    let generatedFor: String?
    let capabilities: [RuntimeCapability]

    enum CodingKeys: String, CodingKey {
        case schema
        case generatedFor = "generated_for"
        case capabilities
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(String.self, forKey: .schema)
        generatedFor = try? c.decode(String.self, forKey: .generatedFor)
        capabilities = try c.decode([RuntimeCapability].self, forKey: .capabilities)
    }

    /// Decode a report from the raw `opensks capability report` JSON.
    static func decode(from data: Data) -> RuntimeCapabilityReport? {
        try? JSONDecoder().decode(RuntimeCapabilityReport.self, from: data)
    }

    /// Run the bundled CLI `capability report` and decode it. Returns `nil` if
    /// the CLI is unavailable or the output cannot be parsed (the caller shows a
    /// truthful "unavailable" state rather than inventing data).
    static func load(cli: URL, workspace: URL) async -> RuntimeCapabilityReport? {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                let process = Process()
                process.executableURL = cli
                process.arguments = ["capability", "report"]
                process.currentDirectoryURL = workspace
                let out = Pipe()
                process.standardOutput = out
                process.standardError = Pipe()
                do {
                    try process.run()
                } catch {
                    continuation.resume(returning: nil)
                    return
                }
                let data = out.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                continuation.resume(returning: RuntimeCapabilityReport.decode(from: data))
            }
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
