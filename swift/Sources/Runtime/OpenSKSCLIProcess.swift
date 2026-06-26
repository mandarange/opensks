// OpenSKSCLIProcess.swift — launch policy for bundled `opensks-cli` children.
// The workspace is carried explicitly so macOS app launches do not need to
// resolve a protected Desktop/Documents cwd before CLI argument parsing.

import Foundation

enum OpenSKSCLIProcess {
    static let workspaceEnvironmentKey = "OPENSKS_WORKSPACE"
    static let commandTimeoutSeconds = 12.0

    static func workingDirectory(for _: URL) -> URL {
        FileManager.default.temporaryDirectory
    }

    static func environment(for workspace: URL) -> [String: String] {
        var environment = ProcessInfo.processInfo.environment
        environment[workspaceEnvironmentKey] = workspace.path
        return environment
    }

    static func environmentOverlay(for workspace: URL) -> [String: String] {
        [workspaceEnvironmentKey: workspace.path]
    }
}
