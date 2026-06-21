// Backend.swift — the concurrency core. A nonisolated CLIRunner actor owns the
// Process + pipe reads and yields Sendable events over an AsyncStream; the
// @MainActor AppState is the single source of truth that views observe. All
// Process work is off the main actor; every UI mutation is on the main actor.

import SwiftUI
import Foundation
import AppKit

// MARK: - Streamed output

enum LineKind: Sendable {
    case cmd, info, warn, danger, done

    var color: Color {
        switch self {
        case .cmd: return Theme.accent
        case .info: return Theme.textSoft
        case .warn: return Theme.gold
        case .danger: return Theme.coral
        case .done: return Theme.faint
        }
    }
}

struct RunLine: Sendable, Identifiable {
    let id = UUID()
    let text: String
    let kind: LineKind
}

enum RunEvent: Sendable {
    case line(RunLine)
    case finished(Int32)
}

func classifyLine(_ s: String) -> LineKind {
    let l = s.lowercased()
    if l.contains("error") || l.contains("failed") || l.contains("panic") { return .danger }
    if l.contains("warn") || l.contains("partial") { return .warn }
    return .info
}

// MARK: - Process runner (off the main actor)

actor CLIRunner {
    /// Run `cli args` capturing all stdout. Used for the quick `app-data` read.
    func capture(cli: URL, cwd: URL, args: [String]) -> Data {
        let proc = Process()
        proc.executableURL = cli
        proc.arguments = args
        proc.currentDirectoryURL = cwd
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = Pipe()
        do { try proc.run() } catch { return Data() }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()
        return data
    }

    /// Run `cli args` streaming stdout/stderr line-by-line as events.
    nonisolated func stream(cli: URL, cwd: URL, args: [String]) -> AsyncStream<RunEvent> {
        AsyncStream { continuation in
            let proc = Process()
            proc.executableURL = cli
            proc.arguments = args
            proc.currentDirectoryURL = cwd
            let outPipe = Pipe()
            let errPipe = Pipe()
            proc.standardOutput = outPipe
            proc.standardError = errPipe

            outPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return }
                for piece in text.split(separator: "\n", omittingEmptySubsequences: false) where !piece.isEmpty {
                    let s = String(piece)
                    continuation.yield(.line(RunLine(text: s, kind: classifyLine(s))))
                }
            }
            errPipe.fileHandleForReading.readabilityHandler = { handle in
                let data = handle.availableData
                guard !data.isEmpty, let text = String(data: data, encoding: .utf8) else { return }
                for piece in text.split(separator: "\n", omittingEmptySubsequences: false) where !piece.isEmpty {
                    continuation.yield(.line(RunLine(text: "! " + String(piece), kind: .warn)))
                }
            }
            proc.terminationHandler = { p in
                outPipe.fileHandleForReading.readabilityHandler = nil
                errPipe.fileHandleForReading.readabilityHandler = nil
                continuation.yield(.finished(p.terminationStatus))
                continuation.finish()
            }

            do {
                try proc.run()
            } catch {
                continuation.yield(.line(RunLine(text: "could not start command: \(error.localizedDescription)", kind: .danger)))
                continuation.yield(.finished(-1))
                continuation.finish()
            }
            continuation.onTermination = { _ in
                if proc.isRunning { proc.terminate() }
            }
        }
    }
}

// MARK: - File tree

struct FileNode: Identifiable {
    let id: String
    let name: String
    let isDir: Bool
    var children: [FileNode]?
}

enum FileScanner {
    private static let skip: Set<String> = ["target", "node_modules", ".git", ".opensks", ".sneakoscope", ".build"]
    private static let secretMarkers = [".env", ".key", ".pem", ".p12", ".pfx", "id_rsa", "credentials", ".token", ".secret", "secret", ".keychain"]

    static func scan(_ root: URL, depth: Int = 0) -> [FileNode] {
        guard depth < 6 else { return [] }
        let keys: [URLResourceKey] = [.isDirectoryKey]
        guard let items = try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: keys, options: [.skipsHiddenFiles]
        ) else { return [] }
        var nodes: [FileNode] = []
        for url in items {
            let name = url.lastPathComponent
            let isDir = (try? url.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            if isDir && skip.contains(name) { continue }
            if isDir {
                nodes.append(FileNode(id: url.path, name: name, isDir: true, children: scan(url, depth: depth + 1)))
            } else {
                nodes.append(FileNode(id: url.path, name: name, isDir: false, children: nil))
            }
        }
        return nodes.sorted { a, b in
            a.isDir != b.isDir ? a.isDir : a.name.lowercased() < b.name.lowercased()
        }
    }

    static func looksSecret(_ path: String) -> Bool {
        let file = (path as NSString).lastPathComponent.lowercased()
        return secretMarkers.contains { file.contains($0) }
    }

    static func read(_ path: String) -> String {
        if looksSecret(path) { return "// hidden for safety — this path may contain credentials." }
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)) else {
            return "// could not read file."
        }
        if data.count > 512 * 1024 { return "// file too large to preview (512 KB cap)." }
        if data.prefix(8000).contains(0) { return "// binary file — preview not available." }
        return String(decoding: data, as: UTF8.self)
    }
}

struct FileTab: Identifiable {
    let id = UUID()
    let path: String
    let name: String
    let lang: CodeLang
    let lines: [String]
}

// MARK: - App state (single source of truth)

@MainActor
final class AppState: ObservableObject {
    @Published var data: AppData?
    @Published var loadError: String?

    @Published var lines: [RunLine] = []
    @Published var isRunning = false
    @Published var lastExit: Int32?
    @Published var lastVerb = ""

    @Published var selectedRail: RailSection = .explorer
    @Published var terminalTab: TerminalTab = .output
    @Published var terminalCollapsed = false

    @Published var fileRoots: [FileNode] = []
    @Published var tabs: [FileTab] = []
    @Published var activeTab: UUID?

    @Published var objective = ""
    @Published var runMode: RunMode = .goal
    @Published var focusObjective = false
    @Published var showPalette = false

    let workspace: URL
    let cli: URL
    private let runner = CLIRunner()
    private var runTask: Task<Void, Never>?

    init() {
        var ws = FileManager.default.currentDirectoryPath
        var cliPath = ws + "/target/debug/opensks"
        if let res = Bundle.main.resourceURL {
            if let txt = try? String(contentsOf: res.appendingPathComponent("workspace-path.txt"), encoding: .utf8) {
                let trimmed = txt.trimmingCharacters(in: .whitespacesAndNewlines)
                if !trimmed.isEmpty { ws = trimmed }
            }
            let candidate = res.appendingPathComponent("opensks-cli")
            if FileManager.default.fileExists(atPath: candidate.path) { cliPath = candidate.path }
        }
        self.workspace = URL(fileURLWithPath: ws, isDirectory: true)
        self.cli = URL(fileURLWithPath: cliPath)
        self.fileRoots = FileScanner.scan(self.workspace)
    }

    var activeFileTab: FileTab? {
        guard let id = activeTab else { return nil }
        return tabs.first { $0.id == id }
    }

    func loadData() {
        let cli = self.cli
        let ws = self.workspace
        let runner = self.runner
        Task {
            let raw = await runner.capture(cli: cli, cwd: ws, args: ["app-data", ws.path])
            if raw.isEmpty {
                self.loadError = "opensks-cli app-data returned no output"
                return
            }
            do {
                let decoder = JSONDecoder()
                decoder.keyDecodingStrategy = .convertFromSnakeCase
                self.data = try decoder.decode(AppData.self, from: raw)
                self.loadError = nil
            } catch {
                self.loadError = "could not decode app-data: \(error.localizedDescription)"
            }
        }
    }

    func startRun() {
        let trimmed = objective.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        runVerb(label: "\(runMode.verb) run", args: [runMode.verb, trimmed])
    }

    func runVerb(label: String, args: [String]) {
        guard !isRunning else { return }
        isRunning = true
        lastExit = nil
        lastVerb = label
        terminalCollapsed = false
        terminalTab = .output
        append(RunLine(text: "$ opensks " + args.joined(separator: " "), kind: .cmd))

        let cli = self.cli
        let ws = self.workspace
        let runner = self.runner
        runTask = Task {
            for await event in runner.stream(cli: cli, cwd: ws, args: args) {
                switch event {
                case .line(let line):
                    self.append(line)
                case .finished(let code):
                    self.lastExit = code
                    self.append(RunLine(
                        text: "— finished (exit \(code)) —",
                        kind: code == 0 ? .done : .danger
                    ))
                }
            }
            self.isRunning = false
            self.loadData()
        }
    }

    func runAcceptance() { runVerb(label: "acceptance audit", args: ["acceptance", "audit"]) }

    private func append(_ line: RunLine) {
        lines.append(line)
        if lines.count > 2000 { lines.removeFirst(lines.count - 2000) }
    }

    func clearOutput() { lines.removeAll() }

    func openFile(_ path: String) {
        if let existing = tabs.first(where: { $0.path == path }) {
            activeTab = existing.id
            return
        }
        let content = FileScanner.read(path)
        let tab = FileTab(
            path: path,
            name: (path as NSString).lastPathComponent,
            lang: CodeLang.detect(path),
            lines: content.components(separatedBy: "\n")
        )
        tabs.append(tab)
        if tabs.count > 12 { tabs.removeFirst() }
        activeTab = tab.id
    }

    func closeTab(_ id: UUID) {
        guard let idx = tabs.firstIndex(where: { $0.id == id }) else { return }
        tabs.remove(at: idx)
        if activeTab == id {
            activeTab = tabs.last?.id
        }
    }

    func reveal(_ path: String) {
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }
}
