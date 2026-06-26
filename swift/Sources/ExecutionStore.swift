import Foundation

struct QueueAction: Equatable, Sendable {
    let itemId: String
    let action: String
    let reasonCode: String
}

struct SteeringAction: Equatable, Sendable {
    let runId: String
    let targetId: String?
    let message: String
}

@MainActor
final class ExecutionStore: ObservableObject {
    @Published private(set) var runs: [RunRecord] = []
    @Published private(set) var queueItems: [QueueItemRecord] = []
    @Published private(set) var approvals: [ApprovalRecord] = []
    @Published private(set) var steering: [SteeringRecord] = []

    private var runMap: [String: RunRecord] = [:]
    private var queueMap: [String: QueueItemRecord] = [:]
    private var approvalMap: [String: ApprovalRecord] = [:]
    private var steeringMap: [String: SteeringRecord] = [:]

    func rebuild(from events: [ExecutionEventEnvelope]) {
        runMap.removeAll()
        queueMap.removeAll()
        approvalMap.removeAll()
        steeringMap.removeAll()
        for event in events.sorted(by: {
            $0.runId == $1.runId ? $0.sequence < $1.sequence : $0.runId < $1.runId
        }) {
            apply(event)
        }
        publish()
    }

    func apply(_ event: ExecutionEventEnvelope) {
        let message = event.payload["message"]?.stringValue ?? event.kind.rawValue
        var run = runMap[event.runId] ?? RunRecord(
            id: event.runId,
            state: "unknown",
            lastSequence: 0,
            lastMessage: "",
            evidenceRefs: []
        )
        run.lastSequence = max(run.lastSequence, event.sequence)
        run.lastMessage = message
        run.evidenceRefs = Array(Set(run.evidenceRefs + event.evidenceRefs)).sorted()

        switch event.kind {
        case .runStarted:
            run.state = "running"
        case .runPaused:
            run.state = "paused"
        case .runResumed:
            run.state = "running"
        case .runCancelled:
            run.state = "cancelled"
        case .runCompleted:
            run.state = "completed"
        case .workItemRunning:
            run.state = "running"
        case .workItemCompleted, .verificationPassed:
            run.state = "verifying"
        case .verificationFailed:
            run.state = "blocked"
        case .snapshotWritten:
            run.state = "snapshot"
        default:
            break
        }
        runMap[event.runId] = run

        if let itemId = event.payload["work_item_id"]?.stringValue {
            var item = queueMap[itemId] ?? QueueItemRecord(
                id: itemId,
                runId: event.runId,
                state: "queued",
                priority: Int(event.payload["priority"]?.numberValue ?? 0),
                lastSequence: 0
            )
            item.state = event.payload["to"]?.stringValue ?? stateForWorkKind(event.kind)
            item.lastSequence = max(item.lastSequence, event.sequence)
            queueMap[itemId] = item
        }

        if let approvalId = event.payload["approval_id"]?.stringValue {
            approvalMap[approvalId] = ApprovalRecord(
                id: approvalId,
                runId: event.runId,
                scope: event.payload["scope"]?.stringValue ?? "unknown",
                state: event.payload["state"]?.stringValue ?? "pending",
                lastSequence: event.sequence
            )
        }

        if let steeringId = event.payload["steering_id"]?.stringValue {
            steeringMap[steeringId] = SteeringRecord(
                id: steeringId,
                runId: event.runId,
                message: event.payload["message"]?.stringValue ?? "",
                targetId: event.payload["target_id"]?.stringValue,
                lastSequence: event.sequence
            )
        }
        publish()
    }

    func latestSequence(for runId: String) -> UInt64 {
        runMap[runId]?.lastSequence ?? runs.first(where: { $0.id == runId })?.lastSequence ?? 0
    }

    func queuedAction(_ action: QueueAction) -> ExecutionEventEnvelope {
        ExecutionEventEnvelope(
            schema: "opensks.execution-event-envelope.v1",
            id: "ui-action-\(action.itemId)-\(action.action)",
            runId: "ui-local",
            sequence: 0,
            occurredAt: "ui-local",
            actor: "opensks-studio",
            causationId: nil,
            correlationId: action.itemId,
            kind: .queueActionRequested,
            payload: .object([
                "work_item_id": .string(action.itemId),
                "action": .string(action.action),
                "reason_code": .string(action.reasonCode)
            ]),
            sensitivity: .public,
            evidenceRefs: []
        )
    }

    func steeringAction(_ action: SteeringAction) -> ExecutionEventEnvelope {
        var payload: [String: JSONValue] = [
            "steering_id": .string("steer-\(action.runId)-\(steering.count + 1)"),
            "message": .string(action.message)
        ]
        if let targetId = action.targetId { payload["target_id"] = .string(targetId) }
        return ExecutionEventEnvelope(
            schema: "opensks.execution-event-envelope.v1",
            id: "ui-steering-\(action.runId)-\(steering.count + 1)",
            runId: action.runId,
            sequence: 0,
            occurredAt: "ui-local",
            actor: "opensks-studio",
            causationId: nil,
            correlationId: action.targetId,
            kind: .steeringRequested,
            payload: .object(payload),
            sensitivity: .public,
            evidenceRefs: []
        )
    }

    private func publish() {
        runs = runMap.values.sorted { $0.lastSequence > $1.lastSequence }
        queueItems = queueMap.values.sorted { $0.lastSequence > $1.lastSequence }
        approvals = approvalMap.values.sorted { $0.lastSequence > $1.lastSequence }
        steering = steeringMap.values.sorted { $0.lastSequence > $1.lastSequence }
    }

    private func stateForWorkKind(_ kind: ExecutionEventKind) -> String {
        switch kind {
        case .workItemQueued: return "queued"
        case .workItemLeased: return "leased"
        case .workItemRunning: return "running"
        case .workItemCompleted: return "completed"
        case .runCancelled: return "cancelled"
        default: return "unknown"
        }
    }
}

private extension JSONValue {
    var numberValue: Double? {
        if case .number(let value) = self { return value }
        return nil
    }
}
