// PipelineProjectionStore.swift — the node-level pipeline projection rebuilt
// from streamed `ExecutionEventEnvelope` events (PR-029).
//
// This replaces the buggy `ExecutionStore` fold where a `snapshot_written`
// event overwrote the run state with the literal string "snapshot" and a
// later/lower-information event could downgrade a terminal node.
//
// Two invariants are enforced by construction:
//
//   1. Rebuild == live. Folding ALL of a run's events at once (`rebuild`)
//      yields exactly the same projection as applying them one-by-one
//      (`ingest`). The reducer is a pure function of (previous projection,
//      next event) with no hidden state, so order-preserving incremental
//      application and a single batch fold are equivalent.
//
//   2. A snapshot (or any lower-information event) NEVER erases terminal or
//      meaningful state. Once a node is `succeeded`/`failed`/`cancelled`/
//      `skipped`, or a run is `completed`/`failed`/`cancelled`, a later
//      snapshot or generic event cannot downgrade it. State only moves
//      "forward" along an explicit rank ladder.
//
// snake_case decoding is consistent with the rest of the app
// (`JSONDecoder.opensks` / `.convertFromSnakeCase`).

import Foundation

// MARK: - Codable projection types (snake_case wire shape)

/// One node's execution state inside a pipeline run.
/// Mirrors `NodeExecutionProjection` of the shared PR-029 shape.
struct NodeExecutionProjection: Codable, Sendable, Equatable, Identifiable {
    var nodeId: String
    var state: NodeProjectionState
    var providerRef: String?
    var modelRef: String?
    var attempt: UInt32
    var touchedPaths: [String]
    var lastPublicMessage: String?

    var id: String { nodeId }

    init(
        nodeId: String,
        state: NodeProjectionState = .queued,
        providerRef: String? = nil,
        modelRef: String? = nil,
        attempt: UInt32 = 0,
        touchedPaths: [String] = [],
        lastPublicMessage: String? = nil
    ) {
        self.nodeId = nodeId
        self.state = state
        self.providerRef = providerRef
        self.modelRef = modelRef
        self.attempt = attempt
        self.touchedPaths = touchedPaths
        self.lastPublicMessage = lastPublicMessage
    }
}

/// Run-level metrics derived from the node set (not retained as raw events).
struct PipelineProjectionMetrics: Codable, Sendable, Equatable {
    var completed: UInt64
    var active: UInt64
    var queued: UInt64
    var failed: UInt64

    init(completed: UInt64 = 0, active: UInt64 = 0, queued: UInt64 = 0, failed: UInt64 = 0) {
        self.completed = completed
        self.active = active
        self.queued = queued
        self.failed = failed
    }
}

/// A whole run's node-level projection.
/// Mirrors `PipelineExecutionProjection` of the shared PR-029 shape.
struct PipelineExecutionProjection: Codable, Sendable, Equatable, Identifiable {
    /// Bump `projectionVersion` to force a rebuild-from-events.
    static let schemaID = "opensks.pipeline-execution-projection.v1"
    static let projectionVersion: UInt64 = 1

    var schema: String
    var projectionVersion: UInt64
    var runId: String
    var conversationId: String?
    var pipelineId: String?
    var state: RunProjectionState
    var nodes: [NodeExecutionProjection]
    var metrics: PipelineProjectionMetrics

    var id: String { runId }

    init(runId: String) {
        self.schema = PipelineExecutionProjection.schemaID
        self.projectionVersion = PipelineExecutionProjection.projectionVersion
        self.runId = runId
        self.conversationId = nil
        self.pipelineId = nil
        self.state = .queued
        self.nodes = []
        self.metrics = PipelineProjectionMetrics()
    }
}

// MARK: - Lenient state enums with a monotonic rank ("information level")

/// Run lifecycle state. Lenient: an unrecognized server value never crashes
/// the decoder and is treated as the lowest-information `.queued` rank so it
/// can never downgrade a terminal run.
enum RunProjectionState: String, Codable, Sendable, Equatable, CaseIterable {
    case queued
    case running
    case paused
    case completed
    case failed
    case cancelled

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = RunProjectionState(rawValue: raw) ?? .queued
    }

    /// Monotonic information rank. State only advances to a strictly higher
    /// rank; a lower- or equal-rank event cannot downgrade it. Terminal states
    /// share the top tier so one terminal never clobbers another.
    var rank: Int {
        switch self {
        case .queued: return 0
        case .paused: return 1
        case .running: return 2
        case .completed, .failed, .cancelled: return 3
        }
    }

    var isTerminal: Bool { rank == 3 }

    /// Run state as surfaced by a `StatusPill` (glyph + tint, never colour alone).
    var pillKind: StatusPill.Kind {
        switch self {
        case .queued, .running: return .running
        case .paused: return .warning
        case .completed: return .success
        case .failed: return .danger
        case .cancelled: return .warning
        }
    }

    var displayLabel: String {
        switch self {
        case .queued: return "Queued"
        case .running: return "Running"
        case .paused: return "Paused"
        case .completed: return "Done"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        }
    }
}

/// Per-node lifecycle state. Lenient like `RunProjectionState`.
enum NodeProjectionState: String, Codable, Sendable, Equatable, CaseIterable {
    case queued
    case dispatching
    case running
    case waitingForApproval
    case succeeded
    case failed
    case cancelled
    case skipped

    // Decode the snake_case wire form explicitly so it is independent of the
    // decoder's key strategy (the value here is a single string, not a key).
    var rawValue: String {
        switch self {
        case .queued: return "queued"
        case .dispatching: return "dispatching"
        case .running: return "running"
        case .waitingForApproval: return "waiting_for_approval"
        case .succeeded: return "succeeded"
        case .failed: return "failed"
        case .cancelled: return "cancelled"
        case .skipped: return "skipped"
        }
    }

    init?(rawValue: String) {
        switch rawValue {
        case "queued": self = .queued
        case "dispatching": self = .dispatching
        case "running": self = .running
        case "waiting_for_approval": self = .waitingForApproval
        case "succeeded": self = .succeeded
        case "failed": self = .failed
        case "cancelled": self = .cancelled
        case "skipped": self = .skipped
        default: return nil
        }
    }

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = NodeProjectionState(rawValue: raw) ?? .queued
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    /// Monotonic information rank. Terminal states (succeeded/failed/cancelled/
    /// skipped) share the top tier so a snapshot or generic event can never
    /// downgrade a finished node.
    var rank: Int {
        switch self {
        case .queued: return 0
        case .dispatching: return 1
        case .running: return 2
        case .waitingForApproval: return 3
        case .succeeded, .failed, .cancelled, .skipped: return 4
        }
    }

    var isTerminal: Bool { rank == 4 }

    var pillKind: StatusPill.Kind {
        switch self {
        case .queued, .dispatching: return .neutral
        case .running: return .running
        case .waitingForApproval: return .warning
        case .succeeded: return .success
        case .failed: return .danger
        case .cancelled, .skipped: return .warning
        }
    }

    var displayLabel: String {
        switch self {
        case .queued: return "Queued"
        case .dispatching: return "Dispatching"
        case .running: return "Running"
        case .waitingForApproval: return "Awaiting approval"
        case .succeeded: return "Succeeded"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        case .skipped: return "Skipped"
        }
    }
}

// MARK: - Pure reducer (value type)

/// A deterministic, value-type fold of `ExecutionEventEnvelope` events into a
/// `PipelineExecutionProjection`. The reducer holds NO raw event payloads — it
/// keeps only the derived projection plus the highest sequence it has seen
/// (for dedup). This makes it bounded regardless of stream length.
struct PipelineProjectionReducer: Sendable {
    private(set) var projection: PipelineExecutionProjection
    /// Highest accepted sequence; events at or below this are dedup'd.
    private(set) var lastSequence: UInt64?
    /// Event kinds that were folded but not understood, kept for observability.
    /// Bounded — only the (small) set of distinct kinds, never per-event copies.
    private(set) var unknownKinds: Set<String> = []

    init(runId: String) {
        projection = PipelineExecutionProjection(runId: runId)
    }

    /// Fold one event. Returns `true` if it advanced the projection, `false`
    /// if it was a duplicate/older sequence (ignored) or belonged to another
    /// run. Never panics; unknown kinds are recorded and otherwise ignored.
    @discardableResult
    mutating func apply(_ event: ExecutionEventEnvelope) -> Bool {
        // Out-of-run events are ignored (defensive — a router should not route
        // them here, but the reducer must never corrupt the projection).
        guard event.runId == projection.runId else { return false }

        // Dedup: ignore already-seen or older sequences. The very first event
        // (lastSequence == nil) is always accepted, including sequence 0.
        if let last = lastSequence, event.sequence <= last {
            return false
        }
        lastSequence = event.sequence

        // Pick up identity hints whenever present (never cleared once set).
        if let conv = event.payload["conversation_id"]?.stringValue {
            projection.conversationId = conv
        }
        if let pipeline = event.payload["pipeline_id"]?.stringValue {
            projection.pipelineId = pipeline
        }

        foldRunState(event)
        foldNode(event)
        recomputeMetrics()
        return true
    }

    // MARK: Run-level fold

    private mutating func foldRunState(_ event: ExecutionEventEnvelope) {
        let candidate: RunProjectionState?
        switch event.kind {
        case .runStarted, .runResumed, .workItemRunning, .workItemLeased,
             .workItemQueued, .leaseHeartbeat, .verificationPassed:
            candidate = .running
        case .runPaused:
            candidate = .paused
        case .runCancelled:
            candidate = .cancelled
        case .snapshotWritten:
            // A snapshot may carry an explicit run state in its projection
            // payload. Honour it ONLY through the monotonic raise below — it
            // can never downgrade (this is the bug fix: no literal "snapshot").
            candidate = event.payload["state"]?.stringValue.flatMap(RunProjectionState.init(rawValue:))
        default:
            // approvals, steering, verificationFailed, lease_expired, unknown…
            // contribute messages/nodes but do not assert a run lifecycle here.
            candidate = nil
            if isUnknownKind(event.kind) {
                unknownKinds.insert(event.kind.rawValue)
            }
        }
        if let candidate { raiseRunState(to: candidate) }
    }

    /// Monotonic raise: advance only to a strictly higher information rank.
    /// Among equal-rank terminals, keep the first one observed (a later
    /// snapshot reasserting a different terminal does not flip it).
    private mutating func raiseRunState(to next: RunProjectionState) {
        if next.rank > projection.state.rank {
            projection.state = next
        }
    }

    // MARK: Node-level fold

    private mutating func foldNode(_ event: ExecutionEventEnvelope) {
        // A node is addressed by `node_id`, falling back to `work_item_id` so
        // existing work-item events map onto nodes without a separate channel.
        guard let nodeId = event.payload["node_id"]?.stringValue
            ?? event.payload["work_item_id"]?.stringValue else { return }

        var node = node(for: nodeId)

        // Identity / provenance — set when present, never cleared by a later
        // lower-information event.
        if let provider = event.payload["provider_ref"]?.stringValue { node.providerRef = provider }
        if let model = event.payload["model_ref"]?.stringValue { node.modelRef = model }
        if let attempt = event.payload["attempt"]?.uintValue { node.attempt = max(node.attempt, attempt) }

        // touched_paths accumulate (union, stable-sorted) and are never dropped.
        if let touched = event.payload["touched_paths"]?.stringArrayValue, !touched.isEmpty {
            node.touchedPaths = Array(Set(node.touchedPaths).union(touched)).sorted()
        }

        // last_public_message updates only from public, non-empty messages and
        // only while the node is not already terminal (a finished node keeps
        // its final public message rather than absorbing a later generic one).
        if let message = publicMessage(from: event), !node.state.isTerminal {
            node.lastPublicMessage = message
        }

        if let candidate = nodeState(for: event) {
            raiseNodeState(&node, to: candidate)
        }

        upsert(node)
    }

    private mutating func raiseNodeState(_ node: inout NodeExecutionProjection, to next: NodeProjectionState) {
        if next.rank > node.state.rank {
            node.state = next
        }
    }

    /// Resolve the node state asserted by an event. An explicit `to` field
    /// (work-item transition) wins; otherwise we infer from the kind. A
    /// snapshot only carries node state via its embedded `state`, applied
    /// through the monotonic raise.
    private func nodeState(for event: ExecutionEventEnvelope) -> NodeProjectionState? {
        if let to = event.payload["to"]?.stringValue,
           let explicit = NodeProjectionState(rawValue: to) {
            return explicit
        }
        switch event.kind {
        case .workItemQueued: return .queued
        case .workItemLeased: return .dispatching
        case .workItemRunning: return .running
        case .approvalRequested: return .waitingForApproval
        case .workItemCompleted, .verificationPassed: return .succeeded
        case .verificationFailed, .leaseExpired: return .failed
        case .runCancelled: return .cancelled
        case .snapshotWritten:
            return event.payload["state"]?.stringValue.flatMap(NodeProjectionState.init(rawValue:))
        default:
            return nil
        }
    }

    // MARK: Metrics

    private mutating func recomputeMetrics() {
        var m = PipelineProjectionMetrics()
        for node in projection.nodes {
            switch node.state {
            case .succeeded, .skipped: m.completed += 1
            case .failed, .cancelled: m.failed += 1
            case .dispatching, .running, .waitingForApproval: m.active += 1
            case .queued: m.queued += 1
            }
        }
        projection.metrics = m
    }

    // MARK: Node storage helpers

    private func node(for nodeId: String) -> NodeExecutionProjection {
        projection.nodes.first(where: { $0.nodeId == nodeId })
            ?? NodeExecutionProjection(nodeId: nodeId)
    }

    private mutating func upsert(_ node: NodeExecutionProjection) {
        if let idx = projection.nodes.firstIndex(where: { $0.nodeId == node.nodeId }) {
            projection.nodes[idx] = node
        } else {
            projection.nodes.append(node)
        }
    }

    // MARK: Misc

    private func isUnknownKind(_ kind: ExecutionEventKind) -> Bool {
        if case .unrecognized = kind { return true }
        if case .unknown = kind { return true }
        return false
    }

    /// Extract a public-safe message. Secret/internal events never leak their
    /// payload message into the public projection.
    private func publicMessage(from event: ExecutionEventEnvelope) -> String? {
        guard event.sensitivity == .public else { return nil }
        guard let message = event.payload["message"]?.stringValue,
              !message.isEmpty else { return nil }
        return message
    }
}

// MARK: - Observable store (thin @MainActor wrapper)

/// A `@MainActor` store that owns per-run `PipelineProjectionReducer`s and
/// republishes their projections for the UI. It keeps no raw event log — only
/// the derived projections — so memory is bounded by the number of live runs
/// and their node counts, not by stream length.
@MainActor
final class PipelineProjectionStore: ObservableObject {
    @Published private(set) var projections: [PipelineExecutionProjection] = []

    private var reducers: [String: PipelineProjectionReducer] = [:]

    init() {}

    /// Live ingest: fold a single streamed event into its run's projection.
    func ingest(_ event: ExecutionEventEnvelope) {
        var reducer = reducers[event.runId] ?? PipelineProjectionReducer(runId: event.runId)
        reducer.apply(event)
        reducers[event.runId] = reducer
        publish()
    }

    /// Rebuild: fold a whole batch of events from scratch. Folding all events
    /// at once MUST equal applying them one-by-one (`ingest`).
    func rebuild(from events: [ExecutionEventEnvelope]) {
        reducers.removeAll()
        // Group by run, then fold each run's events in sequence order. Sorting
        // is order-preserving for already-ordered input, so rebuild matches a
        // live stream that arrived in sequence order.
        let byRun = Dictionary(grouping: events, by: { $0.runId })
        for (runId, runEvents) in byRun {
            var reducer = PipelineProjectionReducer(runId: runId)
            for event in runEvents.sorted(by: { $0.sequence < $1.sequence }) {
                reducer.apply(event)
            }
            reducers[runId] = reducer
        }
        publish()
    }

    /// The projection for one run, if any events have been folded for it.
    func projection(for runId: String) -> PipelineExecutionProjection? {
        reducers[runId]?.projection
    }

    /// Highest accepted sequence for a run — the resume cursor for reconnect.
    func latestSequence(for runId: String) -> UInt64? {
        reducers[runId]?.lastSequence
    }

    func nodes(for runId: String) -> [NodeExecutionProjection] {
        reducers[runId]?.projection.nodes ?? []
    }

    func metrics(for runId: String) -> PipelineProjectionMetrics {
        reducers[runId]?.projection.metrics ?? PipelineProjectionMetrics()
    }

    func reset() {
        reducers.removeAll()
        publish()
    }

    private func publish() {
        projections = reducers.values
            .map(\.projection)
            .sorted { $0.runId < $1.runId }
    }
}

// MARK: - JSONValue numeric/array conveniences (local to this file)

private extension JSONValue {
    var uintValue: UInt32? {
        if case .number(let value) = self, value >= 0 { return UInt32(value) }
        return nil
    }

    var stringArrayValue: [String]? {
        if case .array(let items) = self {
            return items.compactMap { $0.stringValue }
        }
        return nil
    }
}
