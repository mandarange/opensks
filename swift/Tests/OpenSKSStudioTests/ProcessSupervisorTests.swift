import Foundation
import XCTest

@testable import OpenSKSStudio

/// Real-subprocess tests for the §19.2 ProcessSupervisor: concurrent drain,
/// stdin, cancellation→terminate, and timeout — all verifiable headlessly.
final class ProcessSupervisorTests: XCTestCase {
    private let supervisor = ProcessSupervisor()

    func testCapturesStdoutAndExitCode() async throws {
        let result = try await supervisor.run(
            ProcessSupervisor.Spec(executable: URL(fileURLWithPath: "/bin/echo"), arguments: ["hello"])
        )
        XCTAssertEqual(result.exitCode, 0)
        XCTAssertEqual(String(decoding: result.stdout, as: UTF8.self), "hello\n")
        XCTAssertFalse(result.timedOut)
    }

    func testFeedsStdin() async throws {
        let payload = "round-trip-through-stdin"
        let result = try await supervisor.run(
            ProcessSupervisor.Spec(
                executable: URL(fileURLWithPath: "/bin/cat"),
                arguments: [],
                stdin: Data(payload.utf8)
            )
        )
        XCTAssertEqual(String(decoding: result.stdout, as: UTF8.self), payload)
    }

    func testLargeOutputDoesNotDeadlock() async throws {
        // 200 KB on stdout: with sequential draining this could deadlock; the
        // concurrent drains must complete it.
        let result = try await supervisor.run(
            ProcessSupervisor.Spec(
                executable: URL(fileURLWithPath: "/usr/bin/head"),
                arguments: ["-c", "200000", "/dev/zero"]
            )
        )
        XCTAssertEqual(result.exitCode, 0)
        XCTAssertEqual(result.stdout.count, 200_000)
        XCTAssertFalse(result.stdoutTruncated)
        XCTAssertFalse(result.stderrTruncated)
    }

    func testBoundedCaptureReportsTruncation() async throws {
        let result = try await supervisor.run(
            ProcessSupervisor.Spec(
                executable: URL(fileURLWithPath: "/usr/bin/head"),
                arguments: ["-c", "2000", "/dev/zero"],
                maxCaptureBytes: 128
            )
        )
        XCTAssertEqual(result.exitCode, 0)
        XCTAssertEqual(result.stdout.count, 128)
        XCTAssertTrue(result.stdoutTruncated)
        XCTAssertFalse(result.stderrTruncated)
    }

    func testTimeoutTerminatesChild() async throws {
        let start = Date()
        let result = try await supervisor.run(
            ProcessSupervisor.Spec(
                executable: URL(fileURLWithPath: "/bin/sleep"),
                arguments: ["5"],
                timeoutSeconds: 0.5
            )
        )
        XCTAssertTrue(result.timedOut)
        XCTAssertLessThan(Date().timeIntervalSince(start), 3.0, "should not wait the full 5s")
    }

    func testCancellationTerminatesChild() async throws {
        let supervisor = self.supervisor
        let task = Task {
            try await supervisor.run(
                ProcessSupervisor.Spec(
                    executable: URL(fileURLWithPath: "/bin/sleep"),
                    arguments: ["30"]
                )
            )
        }
        try await Task.sleep(nanoseconds: 200_000_000)
        let start = Date()
        task.cancel()
        _ = try? await task.value
        XCTAssertLessThan(
            Date().timeIntervalSince(start), 3.0,
            "cancelled child must terminate promptly, not run for 30s"
        )
    }
}
