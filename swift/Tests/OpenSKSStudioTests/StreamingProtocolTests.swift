import XCTest
@testable import OpenSKSStudio

final class StreamingProtocolTests: XCTestCase {
    // Hand-built wire lines matching the Rust EngineStreamFrame serde format.
    private func opened(_ s: String, cursor: UInt64 = 0) -> String {
        "{\"frame_type\":\"stream_opened\",\"schema\":\"opensks.engine-stream-frame.v2\",\"stream_id\":\"\(s)\",\"request_id\":\"r\",\"project_id\":\"p\",\"conversation_id\":\"c\",\"run_id\":null,\"protocol_version\":\"opensks.stream.v2\",\"cursor\":\(cursor)}"
    }
    private func event(_ s: String, cursor: UInt64) -> String {
        "{\"frame_type\":\"event\",\"schema\":\"x\",\"stream_id\":\"\(s)\",\"cursor\":\(cursor),\"event\":{\"k\":\"v\"}}"
    }
    private func heartbeat(_ s: String, cursor: UInt64) -> String {
        "{\"frame_type\":\"heartbeat\",\"schema\":\"x\",\"stream_id\":\"\(s)\",\"cursor\":\(cursor),\"server_time_ms\":123}"
    }
    private func completed(_ s: String, cursor: UInt64) -> String {
        "{\"frame_type\":\"stream_completed\",\"schema\":\"x\",\"stream_id\":\"\(s)\",\"cursor\":\(cursor),\"reason_code\":\"done\"}"
    }

    func testDecodesAndRoutesAStream() {
        var reader = StreamFrameReader()
        let router = MultiStreamRouter()
        for line in [opened("s1"), event("s1", cursor: 1), completed("s1", cursor: 2)] {
            let frame = reader.decode(line: line)
            XCTAssertNotNil(frame)
            router.ingest(frame!)
        }
        XCTAssertEqual(router.streams["s1"]?.events.count, 1)
        XCTAssertEqual(router.streams["s1"]?.terminal, .completed("done"))
        XCTAssertFalse(router.isOpen("s1"))
        XCTAssertTrue(reader.quarantined.isEmpty)
    }

    func testInterleavedStreamsRouteIndependently() {
        var reader = StreamFrameReader()
        let router = MultiStreamRouter()
        let lines = [
            opened("A"), opened("B"),
            event("A", cursor: 1), event("B", cursor: 1),
            completed("B", cursor: 2),
            event("A", cursor: 2), completed("A", cursor: 3),
        ]
        for l in lines { router.ingest(reader.decode(line: l)!) }
        XCTAssertEqual(router.streams["A"]?.events.count, 2)
        XCTAssertEqual(router.streams["B"]?.events.count, 1)
        XCTAssertFalse(router.isOpen("A"))
        XCTAssertFalse(router.isOpen("B"))
    }

    func testDelayedEventsAreNotTruncated() {
        var reader = StreamFrameReader()
        let router = MultiStreamRouter()
        // Heartbeats then a late event then a terminal: nothing is dropped by any
        // silence/quiet-window heuristic (there is none in the v2 path).
        let lines = [
            opened("s"), heartbeat("s", cursor: 1), heartbeat("s", cursor: 2),
            event("s", cursor: 3), completed("s", cursor: 4),
        ]
        for l in lines {
            XCTAssertEqual(router.ingest(reader.decode(line: l)!), .accept)
        }
        XCTAssertEqual(router.streams["s"]?.events.count, 1, "the late event after heartbeats is kept")
        XCTAssertEqual(router.streams["s"]?.terminal, .completed("done"))
    }

    func testOpenStreamWithoutTerminalStaysOpen() {
        var reader = StreamFrameReader()
        let router = MultiStreamRouter()
        for l in [opened("s"), event("s", cursor: 1)] { router.ingest(reader.decode(line: l)!) }
        XCTAssertTrue(router.isOpen("s"), "no terminal frame => still open; no quiet-window completion")
        XCTAssertNil(router.streams["s"]?.terminal)
    }

    func testMalformedLineIsQuarantined() {
        var reader = StreamFrameReader()
        XCTAssertNil(reader.decode(line: "{not json"))
        XCTAssertNil(reader.decode(line: "{\"frame_type\":\"bogus\",\"stream_id\":\"s\",\"cursor\":0}"))
        XCTAssertEqual(reader.quarantined.count, 2)
        // A valid frame still decodes after quarantine — one bad line is contained.
        XCTAssertNotNil(reader.decode(line: opened("s")))
    }

    func testProcessDeathFailsAllPendingStreamsImmediately() {
        let registry = PendingStreamRegistry()
        let a = MockSink()
        let b = MockSink()
        registry.register(streamID: "a", sink: a)
        registry.register(streamID: "b", sink: b)
        XCTAssertEqual(registry.pendingCount, 2)
        registry.failAll(PublicEngineError(code: "process_terminated", message: "daemon died", retryable: true))
        XCTAssertEqual(a.failure?.code, "process_terminated")
        XCTAssertEqual(b.failure?.code, "process_terminated")
        XCTAssertEqual(registry.pendingCount, 0)
    }

    func testTerminalFrameRetiresSink() {
        let registry = PendingStreamRegistry()
        let a = MockSink()
        registry.register(streamID: "a", sink: a)
        registry.deliver(.completed(streamID: "a", cursor: 1, reasonCode: "done"))
        XCTAssertEqual(a.frames.count, 1)
        XCTAssertEqual(registry.pendingCount, 0)
    }

    func testCursorTrackerDedupsAndDetectsGaps() {
        var t = StreamCursorTracker()
        XCTAssertEqual(t.accept(0), .accept)
        XCTAssertEqual(t.accept(1), .accept)
        XCTAssertEqual(t.accept(1), .duplicateOrOld)
        XCTAssertEqual(t.accept(5), .gap(expected: 2, got: 5))
        XCTAssertEqual(t.last, 1)
    }

    private final class MockSink: StreamSink {
        var frames: [EngineStreamFrame] = []
        var failure: PublicEngineError?
        func deliver(_ frame: EngineStreamFrame) { frames.append(frame) }
        func fail(_ error: PublicEngineError) { failure = error }
    }
}
