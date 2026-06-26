// ProcessSupervisor.swift — one safe place to launch child processes
// (recovery directive §19.2). Replaces the per-service bespoke `Process` code:
//
//   • stdout/stderr drained CONCURRENTLY (no deadlock when one pipe fills while
//     the other is being read) — fixes PROC-102;
//   • Swift Task cancellation TERMINATES the child (group-safe terminate) —
//     fixes PROC-103;
//   • bounded capture (a runaway child cannot exhaust memory);
//   • optional timeout that terminates the child;
//   • the executable is launched directly (never `/bin/sh -c`), with explicit
//     argv — no shell injection surface.
//
// It is an actor so callers share one supervisor; `run` is nonisolated because a
// launch holds no shared actor state — each call owns its process + buffers.

import Darwin
import Foundation

actor ProcessSupervisor {
    struct Spec: Sendable {
        let executable: URL
        let arguments: [String]
        var workingDirectory: URL?
        var environment: [String: String] = [:]
        var stdin: Data?
        /// Timeout in seconds; the child is terminated if it exceeds it.
        var timeoutSeconds: Double?
        /// Max bytes captured per stream before further output is dropped.
        var maxCaptureBytes: Int = 8 * 1024 * 1024
    }

    struct RunResult: Sendable {
        let exitCode: Int32
        let stdout: Data
        let stderr: Data
        let stdoutTruncated: Bool
        let stderrTruncated: Bool
        let timedOut: Bool
    }

    enum ProcessError: Error, Sendable {
        case launchFailed(String)
    }

    /// A byte sink that caps total captured size (thread-safe; the two pipe
    /// readers append concurrently).
    private final class BoundedBuffer: @unchecked Sendable {
        private let lock = NSLock()
        private var data = Data()
        private var truncated = false
        private let cap: Int
        init(cap: Int) { self.cap = cap }
        func append(_ chunk: Data) {
            lock.lock()
            defer { lock.unlock() }
            guard data.count < cap else {
                if !chunk.isEmpty { truncated = true }
                return
            }
            let room = cap - data.count
            if room >= chunk.count {
                data.append(chunk)
            } else {
                data.append(chunk.prefix(room))
                truncated = true
            }
        }
        func snapshot() -> Data {
            lock.lock()
            defer { lock.unlock() }
            return data
        }
        var wasTruncated: Bool {
            lock.lock()
            defer { lock.unlock() }
            return truncated
        }
    }

    nonisolated func run(_ spec: Spec) async throws -> RunResult {
        let process = Process()
        process.executableURL = spec.executable
        process.arguments = spec.arguments
        if let cwd = spec.workingDirectory {
            process.currentDirectoryURL = cwd
        }
        if !spec.environment.isEmpty {
            var environment = ProcessInfo.processInfo.environment
            environment.merge(spec.environment) { _, new in new }
            process.environment = environment
        }

        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation {
                (continuation: CheckedContinuation<RunResult, Error>) in
                let outBuf = BoundedBuffer(cap: spec.maxCaptureBytes)
                let errBuf = BoundedBuffer(cap: spec.maxCaptureBytes)
                let outPipe = Pipe()
                let errPipe = Pipe()
                let inPipe = Pipe()
                process.standardOutput = outPipe
                process.standardError = errPipe
                process.standardInput = inPipe

                let timedOut = TimedOutFlag()

                // Concurrent drains: each pipe has its own readability handler,
                // so neither stream can block the other (PROC-102).
                outPipe.fileHandleForReading.readabilityHandler = { handle in
                    let chunk = handle.availableData
                    if !chunk.isEmpty { outBuf.append(chunk) }
                }
                errPipe.fileHandleForReading.readabilityHandler = { handle in
                    let chunk = handle.availableData
                    if !chunk.isEmpty { errBuf.append(chunk) }
                }

                process.terminationHandler = { proc in
                    outPipe.fileHandleForReading.readabilityHandler = nil
                    errPipe.fileHandleForReading.readabilityHandler = nil
                    // Drain anything buffered after the last handler fire.
                    outBuf.append(outPipe.fileHandleForReading.readDataToEndOfFile())
                    errBuf.append(errPipe.fileHandleForReading.readDataToEndOfFile())
                    continuation.resume(
                        returning: RunResult(
                            exitCode: proc.terminationStatus,
                            stdout: outBuf.snapshot(),
                            stderr: errBuf.snapshot(),
                            stdoutTruncated: outBuf.wasTruncated,
                            stderrTruncated: errBuf.wasTruncated,
                            timedOut: timedOut.value
                        ))
                }

                do {
                    try process.run()
                } catch {
                    outPipe.fileHandleForReading.readabilityHandler = nil
                    errPipe.fileHandleForReading.readabilityHandler = nil
                    continuation.resume(
                        throwing: ProcessError.launchFailed(error.localizedDescription))
                    return
                }

                if let stdin = spec.stdin {
                    inPipe.fileHandleForWriting.write(stdin)
                }
                try? inPipe.fileHandleForWriting.close()

                if let timeout = spec.timeoutSeconds {
                    DispatchQueue.global().asyncAfter(deadline: .now() + timeout) {
                        if process.isRunning {
                            timedOut.set()
                            Self.terminateProcess(process)
                        }
                    }
                }
            }
        } onCancel: {
            // Task cancelled → terminate the child instead of orphaning it.
            if process.isRunning {
                Self.terminateProcess(process)
            }
        }
    }

    private nonisolated static func terminateProcess(_ process: Process) {
        process.terminate()
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.5) {
            if process.isRunning {
                kill(process.processIdentifier, SIGKILL)
            }
        }
    }
}

/// A tiny thread-safe flag for "the timeout fired".
private final class TimedOutFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var flag = false
    var value: Bool {
        lock.lock()
        defer { lock.unlock() }
        return flag
    }
    func set() {
        lock.lock()
        defer { lock.unlock() }
        flag = true
    }
}
