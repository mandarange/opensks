use std::path::Path;

use opensks_artifacts::redact_secrets;
use opensks_contracts::{
    CompiledPlan, EXECUTION_EVENT_ENVELOPE_SCHEMA, EventKind, PipelineGraph, SchedulerSnapshot,
    SchedulerWorkItem, Sensitivity, WorkState,
};
use opensks_event_store::{EventStore, EventStoreError};
use opensks_graph::compile_graph;
use opensks_scheduler::{
    ControlledDispatch, DeterministicWorker, DurableScheduler, SchedulerConfig, SchedulerError,
    SteerReceipt, WorkerDispatchReport, make_work_item, recover_work_item_states,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("graph has compile errors")]
    GraphCompileBlocked,
    #[error("scheduler error: {0}")]
    Scheduler(#[from] SchedulerError),
    #[error("event store error: {0}")]
    EventStore(#[from] EventStoreError),
    #[error("unknown pipeline template `{0}`")]
    UnknownTemplate(String),
    #[error("invalid run control event kind `{0}`")]
    InvalidControlKind(String),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct EngineRunPlan {
    pub compiled_plan: CompiledPlan,
    pub work_items: Vec<SchedulerWorkItem>,
}

#[derive(Debug, Clone)]
pub struct EngineRunResult {
    pub run_id: String,
    pub template_id: String,
    pub snapshot: SchedulerSnapshot,
    pub dispatch_report: WorkerDispatchReport,
    pub events: Vec<opensks_contracts::ExecutionEventEnvelope>,
}

#[derive(Debug, Clone)]
pub struct EngineControlResult {
    pub event: opensks_contracts::ExecutionEventEnvelope,
    /// Present for steer commands: the validation receipt for the target.
    pub steer_receipt: Option<SteerReceipt>,
}

#[derive(Debug, Clone)]
pub struct EngineApprovalResult {
    pub event: opensks_contracts::ExecutionEventEnvelope,
}

#[derive(Debug, Clone)]
pub struct EngineApprovalEventInput<'a> {
    pub run_id: &'a str,
    pub kind: EventKind,
    pub approval_id: &'a str,
    pub scope: &'a str,
    pub state: &'a str,
    pub message: &'a str,
    pub reason_code: &'a str,
}

pub fn plan_graph_for_scheduler(
    run_id: &str,
    graph: &PipelineGraph,
) -> Result<EngineRunPlan, EngineError> {
    let compiled_plan = compile_graph(graph);
    if compiled_plan
        .diagnostics
        .iter()
        .any(|item| item.severity == opensks_contracts::DiagnosticSeverity::Error)
    {
        return Err(EngineError::GraphCompileBlocked);
    }
    let mut work_items = Vec::new();
    for template in &compiled_plan.work_templates {
        let dependencies = template
            .dependencies
            .iter()
            .map(|node_id| format!("work-template-{node_id}"))
            .collect();
        let mut item = make_work_item(run_id, &template.id, dependencies);
        item.node_id = template.node_id.clone();
        item.kind = template.kind.clone();
        item.capability_requirements = template.capability_requirements.clone();
        item.requirement_ids = template.requirement_ids.clone();
        item.state = WorkState::Ready;
        work_items.push(item);
    }
    Ok(EngineRunPlan {
        compiled_plan,
        work_items,
    })
}

pub fn simulate_graph_run(
    run_id: &str,
    graph: &PipelineGraph,
    store: &mut EventStore,
) -> Result<opensks_contracts::SchedulerSnapshot, EngineError> {
    let run_plan = plan_graph_for_scheduler(run_id, graph)?;
    let mut scheduler =
        DurableScheduler::new(run_id, run_plan.work_items, SchedulerConfig::default());
    Ok(scheduler.simulate_until_idle(store)?)
}

pub fn dispatch_graph_run(
    run_id: &str,
    graph: &PipelineGraph,
    store: &mut EventStore,
) -> Result<(SchedulerSnapshot, WorkerDispatchReport), EngineError> {
    let run_plan = plan_graph_for_scheduler(run_id, graph)?;
    let mut scheduler =
        DurableScheduler::new(run_id, run_plan.work_items, SchedulerConfig::default());
    let mut worker = DeterministicWorker::new("engine-local-worker");
    Ok(scheduler.dispatch_until_idle(store, &mut worker)?)
}

pub fn run_template_with_event_stream(
    workspace: &Path,
    run_id: &str,
    template_id: &str,
    objective: &str,
) -> Result<EngineRunResult, EngineError> {
    let graph = opensks_graph::default_templates()
        .into_iter()
        .find(|graph| graph.id == template_id)
        .ok_or_else(|| EngineError::UnknownTemplate(template_id.to_string()))?;
    run_graph_with_event_stream(
        workspace,
        run_id,
        &graph,
        objective,
        "daemon:run-start-request",
        "built_in_template",
    )
}

pub fn run_graph_with_event_stream(
    workspace: &Path,
    run_id: &str,
    graph: &PipelineGraph,
    objective: &str,
    request_evidence_ref: &str,
    graph_source: &str,
) -> Result<EngineRunResult, EngineError> {
    let mut store = EventStore::open_workspace(workspace)?;
    let started = opensks_contracts::ExecutionEventEnvelope {
        schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("evt-{run_id}-started"),
        run_id: run_id.to_string(),
        sequence: 0,
        occurred_at: "engine-run-started".to_string(),
        actor: "opensks-engine".to_string(),
        causation_id: None,
        correlation_id: Some(graph.id.clone()),
        kind: EventKind::RunStarted,
        payload: serde_json::json!({
            "message": "run.start accepted",
            "pipeline_id": graph.id,
            "graph_source": graph_source,
            "objective": objective,
        }),
        sensitivity: Sensitivity::Public,
        evidence_refs: vec![request_evidence_ref.to_string()],
    };
    store.append_event(started)?;
    let (snapshot, dispatch_report) = dispatch_graph_run(run_id, graph, &mut store)?;
    let snapshot_event = opensks_contracts::ExecutionEventEnvelope {
        schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("evt-{run_id}-snapshot"),
        run_id: run_id.to_string(),
        sequence: 0,
        occurred_at: "engine-snapshot-written".to_string(),
        actor: "opensks-engine".to_string(),
        causation_id: None,
        correlation_id: Some(graph.id.clone()),
        kind: EventKind::SnapshotWritten,
        payload: serde_json::json!({
            "message": "scheduler snapshot written",
            "work_item_count": snapshot.work_items.len(),
            "max_concurrent_workers": snapshot.max_concurrent_workers,
            "overlap_ratio": snapshot.overlap_ratio,
            "worker_dispatch_attempted": dispatch_report.attempted,
            "worker_dispatch_completed": dispatch_report.completed,
            "worker_dispatch_failed": dispatch_report.failed,
        }),
        sensitivity: Sensitivity::Public,
        evidence_refs: vec![
            "event-store:snapshot-written".to_string(),
            "scheduler:worker-dispatch".to_string(),
        ],
    };
    let snapshot_event = store.append_event(snapshot_event)?;
    store.write_snapshot(
        run_id,
        snapshot_event.sequence,
        serde_json::to_value(&snapshot)?,
    )?;
    let events = store.replay(run_id)?;
    Ok(EngineRunResult {
        run_id: run_id.to_string(),
        template_id: graph.id.clone(),
        snapshot,
        dispatch_report,
        events,
    })
}

pub fn append_run_control_event(
    workspace: &Path,
    run_id: &str,
    kind: EventKind,
    target_id: Option<&str>,
    message: &str,
    reason_code: &str,
) -> Result<EngineControlResult, EngineError> {
    let allowed = matches!(
        kind,
        EventKind::RunPaused
            | EventKind::RunResumed
            | EventKind::RunCancelled
            | EventKind::SteeringRequested
    );
    if !allowed {
        return Err(EngineError::InvalidControlKind(kind.as_str().to_string()));
    }

    let mut store = EventStore::open_workspace(workspace)?;

    // A steer command is validated against the run's recovered work-item states.
    // The receipt is Applied only for a known, non-terminal (steerable) target.
    let steer_receipt = if kind == EventKind::SteeringRequested {
        Some(validate_steer_target(&store, run_id, target_id)?)
    } else {
        None
    };

    let next_sequence = store.next_sequence(run_id)?;
    // The steer message may carry user-provided text; redact secrets before it
    // is persisted to the durable event store.
    let safe_message = redact_secrets(message);
    let mut payload = serde_json::json!({
        "message": safe_message,
        "reason_code": reason_code,
    });
    if let Some(target_id) = target_id {
        payload["target_id"] = serde_json::Value::String(target_id.to_string());
        payload["work_item_id"] = serde_json::Value::String(target_id.to_string());
    }
    if kind == EventKind::SteeringRequested {
        payload["steering_id"] =
            serde_json::Value::String(format!("steer-{run_id}-{next_sequence}"));
        if let Some(receipt) = &steer_receipt {
            payload["steer_receipt"] = serde_json::to_value(receipt)?;
        }
    }

    let event = opensks_contracts::ExecutionEventEnvelope {
        schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("evt-{run_id}-control-{next_sequence}"),
        run_id: run_id.to_string(),
        sequence: 0,
        occurred_at: "engine-run-control".to_string(),
        actor: "opensks-engine".to_string(),
        causation_id: None,
        correlation_id: target_id.map(str::to_string),
        kind,
        payload,
        sensitivity: Sensitivity::Public,
        evidence_refs: vec!["daemon:run-control-request".to_string()],
    };
    let event = store.append_event(event)?;
    Ok(EngineControlResult {
        event,
        steer_receipt,
    })
}

/// Validate a steer target against a run's durable, replayed work-item states.
fn validate_steer_target(
    store: &EventStore,
    run_id: &str,
    target_id: Option<&str>,
) -> Result<SteerReceipt, EngineError> {
    let Some(target_id) = target_id else {
        return Ok(SteerReceipt::Rejected {
            target_id: String::new(),
            reason: "missing_target_id".to_string(),
        });
    };
    let states = recover_work_item_states(store, run_id)?;
    let receipt = match states.get(target_id) {
        None => SteerReceipt::Rejected {
            target_id: target_id.to_string(),
            reason: "unknown_work_item".to_string(),
        },
        Some(state) if state.is_terminal() => SteerReceipt::Rejected {
            target_id: target_id.to_string(),
            reason: format!("work_item_terminal:{state:?}"),
        },
        Some(_) => SteerReceipt::Applied {
            target_id: target_id.to_string(),
        },
    };
    Ok(receipt)
}

/// Dispatch a graph run while honoring the durable control mailbox.
///
/// A run whose control events already carry a `Cancel` or `Pause` does not
/// dispatch new work: cancel transitions still-queued items to `Cancelled`;
/// pause quiesces to the true `paused` state. With no control events this is
/// equivalent to [`dispatch_graph_run`].
pub fn dispatch_graph_run_with_control(
    run_id: &str,
    graph: &PipelineGraph,
    store: &mut EventStore,
) -> Result<ControlledDispatch, EngineError> {
    let run_plan = plan_graph_for_scheduler(run_id, graph)?;
    let mut scheduler =
        DurableScheduler::new(run_id, run_plan.work_items, SchedulerConfig::default());
    let mut worker = DeterministicWorker::new("engine-local-worker");
    Ok(scheduler.dispatch_until_idle_with_control(store, &mut worker)?)
}

pub fn append_approval_event(
    workspace: &Path,
    input: EngineApprovalEventInput<'_>,
) -> Result<EngineApprovalResult, EngineError> {
    let allowed = matches!(
        input.kind,
        EventKind::ApprovalRequested | EventKind::ApprovalApproved | EventKind::ApprovalDenied
    );
    if !allowed {
        return Err(EngineError::InvalidControlKind(
            input.kind.as_str().to_string(),
        ));
    }
    let mut store = EventStore::open_workspace(workspace)?;
    let next_sequence = store.next_sequence(input.run_id)?;
    let event = opensks_contracts::ExecutionEventEnvelope {
        schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("evt-{}-approval-{next_sequence}", input.run_id),
        run_id: input.run_id.to_string(),
        sequence: 0,
        occurred_at: "engine-approval".to_string(),
        actor: "opensks-engine".to_string(),
        causation_id: None,
        correlation_id: Some(input.approval_id.to_string()),
        kind: input.kind,
        payload: serde_json::json!({
            "approval_id": input.approval_id,
            "scope": input.scope,
            "state": input.state,
            "message": input.message,
            "reason_code": input.reason_code,
        }),
        sensitivity: Sensitivity::Public,
        evidence_refs: vec!["daemon:approval-request".to_string()],
    };
    let event = store.append_event(event)?;
    Ok(EngineApprovalResult { event })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_compiles_graph_into_scheduler_items() {
        let graph = opensks_graph::single_model_safe_template();
        let run_plan = plan_graph_for_scheduler("run-engine", &graph).expect("run plan");
        assert!(!run_plan.work_items.is_empty());
        assert_eq!(run_plan.compiled_plan.graph_id, "single-model-safe");
    }

    #[test]
    fn engine_can_simulate_default_graph() {
        let graph = opensks_graph::single_model_safe_template();
        let mut store = EventStore::open_memory().expect("store");
        let snapshot =
            simulate_graph_run("run-engine-sim", &graph, &mut store).expect("simulate graph");
        assert!(
            snapshot
                .work_items
                .iter()
                .all(|item| item.state == WorkState::Completed)
        );
    }

    #[test]
    fn engine_can_dispatch_default_graph() {
        let graph = opensks_graph::single_model_safe_template();
        let mut store = EventStore::open_memory().expect("store");
        let (snapshot, report) =
            dispatch_graph_run("run-engine-dispatch", &graph, &mut store).expect("dispatch graph");
        assert_eq!(report.failed, 0);
        assert_eq!(report.completed, snapshot.work_items.len());
        assert!(
            snapshot
                .evidence_refs
                .contains(&"scheduler:worker-dispatch".to_string())
        );
    }

    #[test]
    fn engine_run_template_emits_replayable_event_stream() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-engine-run-template-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let result = run_template_with_event_stream(
            &workspace,
            "run-engine-template",
            "single-model-safe",
            "prove run.start",
        )
        .expect("run template");
        assert_eq!(result.run_id, "run-engine-template");
        assert_eq!(result.template_id, "single-model-safe");
        assert!(
            result
                .snapshot
                .work_items
                .iter()
                .all(|item| { item.state == WorkState::Completed })
        );
        assert!(
            result
                .events
                .iter()
                .any(|event| event.kind == EventKind::RunStarted)
        );
        assert!(
            result
                .events
                .iter()
                .any(|event| event.kind == EventKind::SnapshotWritten)
        );
        assert_eq!(result.dispatch_report.failed, 0);
        assert!(result.events.iter().any(|event| {
            event.kind == EventKind::SnapshotWritten
                && event.payload["worker_dispatch_completed"]
                    .as_u64()
                    .unwrap_or(0)
                    > 0
                && event
                    .evidence_refs
                    .contains(&"scheduler:worker-dispatch".to_string())
        }));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn engine_runs_custom_pipeline_graph_event_stream() {
        let workspace =
            std::env::temp_dir().join(format!("opensks-engine-run-graph-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut graph = opensks_graph::single_model_safe_template();
        graph.id = "editor-draft".to_string();
        graph.name = "Editor Draft".to_string();
        let result = run_graph_with_event_stream(
            &workspace,
            "run-engine-graph",
            &graph,
            "prove graph path",
            "daemon:graph-path-run-start-request",
            "workspace_graph_path",
        )
        .expect("run graph");
        assert_eq!(result.run_id, "run-engine-graph");
        assert_eq!(result.template_id, "editor-draft");
        assert!(
            result
                .events
                .iter()
                .any(|event| event.kind == EventKind::RunStarted
                    && event.payload["graph_source"] == "workspace_graph_path")
        );
        assert!(
            result
                .snapshot
                .work_items
                .iter()
                .all(|item| item.state == WorkState::Completed)
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn engine_appends_run_control_events_to_store() {
        let workspace =
            std::env::temp_dir().join(format!("opensks-engine-run-control-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let started = run_template_with_event_stream(
            &workspace,
            "run-control",
            "single-model-safe",
            "prove run control",
        )
        .expect("start run");
        assert!(!started.events.is_empty());

        let cancelled = append_run_control_event(
            &workspace,
            "run-control",
            EventKind::RunCancelled,
            None,
            "run cancel requested",
            "cancelled_by_user",
        )
        .expect("cancel event");
        assert_eq!(cancelled.event.kind, EventKind::RunCancelled);
        assert!(
            cancelled
                .event
                .evidence_refs
                .contains(&"daemon:run-control-request".to_string())
        );

        let steered = append_run_control_event(
            &workspace,
            "run-control",
            EventKind::SteeringRequested,
            Some("work-template-delegate"),
            "focus the delegate on tests",
            "user_steering",
        )
        .expect("steer event");
        assert_eq!(steered.event.kind, EventKind::SteeringRequested);
        assert_eq!(
            steered.event.payload["work_item_id"],
            "work-template-delegate"
        );
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn engine_steer_returns_applied_or_rejected_receipt() {
        // Acceptance criterion 4 (engine boundary): append_run_control_event
        // RETURNS a typed steer receipt; we assert the receipt, not just an event.
        let workspace =
            std::env::temp_dir().join(format!("opensks-engine-steer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");

        // Start a run, then derive a steerable target from its work items.
        let mut store = EventStore::open_workspace(&workspace).expect("store");
        let graph = opensks_graph::single_model_safe_template();
        let run_plan = plan_graph_for_scheduler("run-steer", &graph).expect("plan");
        let mut scheduler =
            DurableScheduler::new("run-steer", run_plan.work_items, SchedulerConfig::default());
        // Lease a ready item so it has a non-terminal recovered state.
        let target_id = scheduler
            .ready_items()
            .first()
            .cloned()
            .expect("ready item");
        scheduler
            .lease_ready_item(&mut store, &target_id, "worker")
            .expect("lease target");
        drop(store);

        let applied = append_run_control_event(
            &workspace,
            "run-steer",
            EventKind::SteeringRequested,
            Some(&target_id),
            "focus on tests",
            "user_steering",
        )
        .expect("steer applied");
        assert_eq!(
            applied.steer_receipt,
            Some(SteerReceipt::Applied {
                target_id: target_id.clone()
            })
        );

        let rejected = append_run_control_event(
            &workspace,
            "run-steer",
            EventKind::SteeringRequested,
            Some("not-a-real-item"),
            "steer ghost",
            "user_steering",
        )
        .expect("steer rejected");
        assert!(matches!(
            rejected.steer_receipt,
            Some(SteerReceipt::Rejected { reason, .. }) if reason == "unknown_work_item"
        ));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn engine_steer_message_is_redacted_before_persistence() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-engine-steer-redact-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        run_template_with_event_stream(
            &workspace,
            "run-redact",
            "single-model-safe",
            "prove redaction",
        )
        .expect("start run");

        let steered = append_run_control_event(
            &workspace,
            "run-redact",
            EventKind::SteeringRequested,
            Some("work-template-delegate"),
            "use sk-supersecret-token now",
            "user_steering",
        )
        .expect("steer event");
        let message = steered.event.payload["message"].as_str().unwrap();
        assert!(!message.contains("sk-supersecret-token"));
        assert!(message.contains("[redacted]"));
        let _ = std::fs::remove_dir_all(&workspace);
    }

    #[test]
    fn engine_prior_cancel_blocks_dispatch() {
        // Acceptance criterion 5: a run started with a prior Cancel in its
        // control events does not dispatch new work.
        let mut store = EventStore::open_memory().expect("store");
        let graph = opensks_graph::single_model_safe_template();
        // Record a cancel into the durable mailbox before dispatch.
        let next_sequence = store.next_sequence("run-precancel").expect("seq");
        let event = opensks_contracts::ExecutionEventEnvelope {
            schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!("evt-run-precancel-control-{next_sequence}"),
            run_id: "run-precancel".to_string(),
            sequence: 0,
            occurred_at: "engine-run-control".to_string(),
            actor: "opensks-engine".to_string(),
            causation_id: None,
            correlation_id: None,
            kind: EventKind::RunCancelled,
            payload: serde_json::json!({
                "message": "cancel",
                "reason_code": "cancelled_by_user",
            }),
            sensitivity: Sensitivity::Public,
            evidence_refs: vec!["daemon:run-control-request".to_string()],
        };
        store.append_event(event).expect("append cancel");

        let controlled = dispatch_graph_run_with_control("run-precancel", &graph, &mut store)
            .expect("controlled dispatch");
        assert_eq!(
            controlled.control_state,
            opensks_scheduler::ExecutionControlState::Cancelled
        );
        assert_eq!(controlled.report.completed, 0);
        assert!(
            controlled
                .snapshot
                .work_items
                .iter()
                .all(|item| item.state == WorkState::Cancelled)
        );
    }

    #[test]
    fn engine_no_control_dispatch_runs_normally() {
        // Acceptance criterion 5 (normal path preserved): with no control events,
        // the control-aware dispatch behaves like a plain dispatch.
        let mut store = EventStore::open_memory().expect("store");
        let graph = opensks_graph::single_model_safe_template();
        let controlled = dispatch_graph_run_with_control("run-normal", &graph, &mut store)
            .expect("controlled dispatch");
        assert_eq!(
            controlled.control_state,
            opensks_scheduler::ExecutionControlState::Running
        );
        assert_eq!(
            controlled.report.completed,
            controlled.snapshot.work_items.len()
        );
        assert!(
            controlled
                .snapshot
                .work_items
                .iter()
                .all(|item| item.state == WorkState::Completed)
        );
    }

    #[test]
    fn engine_appends_approval_events_to_store() {
        let workspace =
            std::env::temp_dir().join(format!("opensks-engine-approval-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        run_template_with_event_stream(
            &workspace,
            "run-approval",
            "single-model-safe",
            "prove approval events",
        )
        .expect("start run");
        let requested = append_approval_event(
            &workspace,
            EngineApprovalEventInput {
                run_id: "run-approval",
                kind: EventKind::ApprovalRequested,
                approval_id: "approval-1",
                scope: "git_push",
                state: "pending",
                message: "approve git push",
                reason_code: "approval_required",
            },
        )
        .expect("approval requested");
        assert_eq!(requested.event.kind, EventKind::ApprovalRequested);
        assert_eq!(requested.event.payload["approval_id"], "approval-1");

        let approved = append_approval_event(
            &workspace,
            EngineApprovalEventInput {
                run_id: "run-approval",
                kind: EventKind::ApprovalApproved,
                approval_id: "approval-1",
                scope: "git_push",
                state: "approved",
                message: "approved",
                reason_code: "approved_by_user",
            },
        )
        .expect("approval approved");
        assert_eq!(approved.event.kind, EventKind::ApprovalApproved);
        assert_eq!(approved.event.payload["state"], "approved");
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
