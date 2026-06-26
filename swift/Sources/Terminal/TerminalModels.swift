import Foundation
import SwiftUI

let terminalMaxOutputPreviewCharacters = 20_000
let terminalMaxBlocksInMemory = 200

struct UnknownPreservingLabel: Equatable, Codable {
    let rawValue: String
}

struct TerminalSessionState: Equatable, Identifiable {
    let id: String
    var cwd: String
    var shell: String
    var status: TerminalSessionStatus
    var startedAt: Date?
    var lastExitCode: Int?
}

enum TerminalSessionStatus: String, Codable {
    case disconnected
    case starting
    case running
    case stopping
    case exited
    case failed
}

struct TerminalCommandBlockModel: Identifiable, Equatable {
    let id: String
    var commandRedacted: String
    var outputPreview: String
    var exitCode: Int?
    var startedAtMs: UInt64
    var finishedAtMs: UInt64?
    var redacted: Bool

    var isRunning: Bool { finishedAtMs == nil }
}

struct TerminalSuggestionModel: Identifiable, Equatable, Codable {
    let id: String
    var replacement: String
    var display: String
    var description: String
    var source: String
    var confidence: Double
    var risk: TerminalRiskLevel
    var requiresApproval: Bool

    enum CodingKeys: String, CodingKey {
        case id
        case replacement
        case display
        case command
        case commandRedacted = "command_redacted"
        case description
        case reason
        case source
        case confidence
        case risk
        case requiresApproval = "requires_approval"
    }

    init(
        id: String,
        replacement: String,
        display: String,
        description: String,
        source: String,
        confidence: Double,
        risk: TerminalRiskLevel,
        requiresApproval: Bool
    ) {
        self.id = id
        self.replacement = replacement
        self.display = display
        self.description = description
        self.source = source
        self.confidence = confidence
        self.risk = risk
        self.requiresApproval = requiresApproval
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let replacement = try container.decodeIfPresent(String.self, forKey: .replacement)
            ?? container.decodeIfPresent(String.self, forKey: .command)
            ?? ""
        let display = try container.decodeIfPresent(String.self, forKey: .display)
            ?? container.decodeIfPresent(String.self, forKey: .commandRedacted)
            ?? replacement
        let risk = try container.decodeIfPresent(TerminalRiskLevel.self, forKey: .risk) ?? .unknown
        let requiresApproval = try container.decodeIfPresent(Bool.self, forKey: .requiresApproval)
            ?? risk.requiresApprovalByDefault
        self.id = try container.decodeIfPresent(String.self, forKey: .id) ?? "suggestion-\(replacement)"
        self.replacement = replacement
        self.display = display
        self.description = try container.decodeIfPresent(String.self, forKey: .description)
            ?? container.decodeIfPresent(String.self, forKey: .reason)
            ?? ""
        self.source = try container.decodeIfPresent(String.self, forKey: .source) ?? "daemon"
        self.confidence = try container.decodeIfPresent(Double.self, forKey: .confidence) ?? 0
        self.risk = risk
        self.requiresApproval = requiresApproval
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(replacement, forKey: .replacement)
        try container.encode(display, forKey: .display)
        try container.encode(description, forKey: .description)
        try container.encode(source, forKey: .source)
        try container.encode(confidence, forKey: .confidence)
        try container.encode(risk, forKey: .risk)
        try container.encode(requiresApproval, forKey: .requiresApproval)
    }
}

enum TerminalRiskLevel: RawRepresentable, Codable, Equatable {
    case safe
    case caution
    case destructive
    case privileged
    case secretExposure
    case networkMutation
    case unknown

    init(rawValue: String) {
        switch rawValue {
        case "safe": self = .safe
        case "caution": self = .caution
        case "destructive": self = .destructive
        case "privileged": self = .privileged
        case "secret_exposure": self = .secretExposure
        case "network_mutation": self = .networkMutation
        default: self = .unknown
        }
    }

    var rawValue: String {
        switch self {
        case .safe: return "safe"
        case .caution: return "caution"
        case .destructive: return "destructive"
        case .privileged: return "privileged"
        case .secretExposure: return "secret_exposure"
        case .networkMutation: return "network_mutation"
        case .unknown: return "unknown"
        }
    }

    var requiresApprovalByDefault: Bool {
        switch self {
        case .safe, .caution:
            return false
        case .destructive, .privileged, .secretExposure, .networkMutation, .unknown:
            return true
        }
    }

    var blockedByDefault: Bool {
        self == .destructive || self == .secretExposure
    }

    var displayLabel: String {
        switch self {
        case .safe: return "safe"
        case .caution: return "caution"
        case .destructive: return "destructive"
        case .privileged: return "privileged"
        case .secretExposure: return "secret exposure"
        case .networkMutation: return "network mutation"
        case .unknown: return "unknown"
        }
    }
}

struct TerminalAgentMessage: Identifiable, Equatable {
    let id: String
    var text: String
    var createdAtMs: UInt64
    var isError: Bool
}

enum TerminalDaemonStatus: String, Equatable {
    case unknown
    case unavailable
    case starting
    case healthy
    case providerUnavailable
    case unsupportedPlatform

    var label: String {
        switch self {
        case .unknown: return "unknown"
        case .unavailable: return "unavailable"
        case .starting: return "starting"
        case .healthy: return "healthy"
        case .providerUnavailable: return "provider unavailable"
        case .unsupportedPlatform: return "unsupported"
        }
    }

    var pillKind: StatusPill.Kind {
        switch self {
        case .healthy:
            return .success
        case .starting:
            return .running
        case .providerUnavailable:
            return .warning
        case .unavailable, .unsupportedPlatform:
            return .danger
        case .unknown:
            return .neutral
        }
    }
}

enum TerminalInputClassifier {
    static func isLikelyNaturalLanguage(_ text: String) -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return false }
        if trimmed.hasPrefix("/") || trimmed.hasPrefix("!") { return false }
        let firstToken = trimmed.split(separator: " ").first.map(String.init)?.lowercased() ?? ""
        let shellCommands: Set<String> = [
            "cat", "cd", "chmod", "cp", "curl", "echo", "find", "git", "grep", "ls", "make",
            "mkdir", "mv", "node", "npm", "pnpm", "python", "python3", "rg", "rm", "sh",
            "swift", "swiftc", "tar", "touch", "vim", "zsh", "cargo"
        ]
        if shellCommands.contains(firstToken) { return false }
        if trimmed.contains("|") || trimmed.contains("&&") || trimmed.contains(";") { return false }
        if trimmed.contains("?") { return true }
        if trimmed.range(of: #"[가-힣]"#, options: .regularExpression) != nil { return true }
        let words = trimmed.split(whereSeparator: { $0 == " " || $0 == "\t" })
        return words.count >= 3
    }

    static func localRisk(for command: String) -> TerminalRiskLevel {
        let lowered = command.lowercased()
        if lowered.contains(".env") || lowered.contains("id_rsa") || lowered.contains("secret") {
            return .secretExposure
        }
        if lowered.contains("rm -rf") || lowered.contains("diskutil erase") || lowered.contains("mkfs") {
            return .destructive
        }
        if lowered.hasPrefix("sudo ") || lowered.contains(" chmod 777") {
            return .privileged
        }
        if lowered.hasPrefix("git push") || lowered.contains("curl -x post") || lowered.contains("curl -d") {
            return .networkMutation
        }
        if command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return .unknown
        }
        return .safe
    }

    static func redacted(_ command: String) -> (text: String, redacted: Bool) {
        let patterns = [
            #"(?i)(api[_-]?key|token|password|secret)=\S+"#,
            #"(?i)(bearer\s+)[A-Za-z0-9._\-]+"#
        ]
        var next = command
        var changed = false
        for pattern in patterns {
            if let regex = try? NSRegularExpression(pattern: pattern) {
                let range = NSRange(next.startIndex..<next.endIndex, in: next)
                let replaced = regex.stringByReplacingMatches(
                    in: next,
                    options: [],
                    range: range,
                    withTemplate: "$1[redacted]"
                )
                if replaced != next { changed = true }
                next = replaced
            }
        }
        return (next, changed)
    }
}

enum TerminalPreviewSanitizer {
    static func plainPreview(from text: String, limit: Int = terminalMaxOutputPreviewCharacters) -> String {
        let withoutANSI = text.replacingOccurrences(
            of: #"\u{001B}\[[0-9;?]*[ -/]*[@-~]"#,
            with: "",
            options: .regularExpression
        )
        let redacted = withoutANSI.replacingOccurrences(
            of: #"(?i)(api[_-]?key|token|password|secret)=\S+"#,
            with: "$1=[redacted]",
            options: .regularExpression
        )
        if redacted.count <= limit { return redacted }
        return String(redacted.suffix(limit))
    }
}

struct TerminalSessionStartRequest: Encodable {
    let schema = "opensks.terminal-session-start.v1"
    let requestId: String
    let sessionId: String
    let cwd: String
    let shell: String?
}

struct TerminalInputRequest: Encodable {
    let schema = "opensks.terminal-input.v1"
    let requestId: String
    let sessionId: String
    let text: String
    let inputKind: String
}

struct TerminalResizeRequest: Encodable {
    let schema = "opensks.terminal-resize.v1"
    let requestId: String
    let sessionId: String
    let cols: Int
    let rows: Int
}

struct TerminalSessionStopRequest: Encodable {
    let schema = "opensks.terminal-session-stop.v1"
    let requestId: String
    let sessionId: String
}

struct TerminalSuggestionRequest: Encodable {
    let schema = "opensks.terminal-suggestion-request.v1"
    let requestId: String
    let cwd: String
    let input: String
    let cursor: Int
    let maxSuggestions: Int
    let includeAI: Bool
    let contextRefs: [String]
}

struct TerminalAgentTurnStartRequest: Encodable {
    let schema = "opensks.terminal-agent-turn-start.v1"
    let requestId: String
    let prompt: String
    let sessionId: String
    let cwd: String
    let maxSuggestions: Int
    let contextRefs: [String]
}
