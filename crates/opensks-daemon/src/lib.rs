use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opensks_contracts::{
    EngineEvent, EngineEventType, EngineRequest, EngineRequestKind, EventKind, EventSeverity,
    PipelineGraph,
};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub workspace: PathBuf,
}

const SUBSCRIPTION_TAIL_MAX_MS: u64 = 5_000;
const SUBSCRIPTION_TAIL_DEFAULT_POLL_MS: u64 = 100;
const SUBSCRIPTION_TAIL_MIN_POLL_MS: u64 = 10;
const SUBSCRIPTION_TAIL_MAX_POLL_MS: u64 = 500;
const MAX_PENDING_STREAM_REQUESTS: usize = 32;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("could not encode daemon event: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("stdio error: {0}")]
    Io(#[from] std::io::Error),
    #[error("stream response channel closed")]
    StreamClosed,
    #[error("stream worker panicked")]
    StreamWorkerPanic,
}

enum StreamResponse {
    Lines(Vec<String>),
}

pub fn run_stdio(input: &str, options: &DaemonOptions) -> Result<String, DaemonError> {
    let mut lines = Vec::new();
    lines.push(serde_json::to_string(&hello_event(None, options))?);

    if input.trim().is_empty() {
        lines.push(serde_json::to_string(&health_event(None, options))?);
        return Ok(lines.join("\n") + "\n");
    }

    for (idx, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        lines.extend(request_lines(idx, trimmed, options)?);
    }

    Ok(lines.join("\n") + "\n")
}

pub fn run_stdio_stream<R: BufRead, W: Write + Send>(
    mut reader: R,
    mut writer: W,
    options: &DaemonOptions,
) -> Result<(), DaemonError> {
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<StreamResponse>();
        let writer_handle = scope.spawn(move || -> Result<(), DaemonError> {
            for response in rx {
                match response {
                    StreamResponse::Lines(lines) => {
                        for output_line in lines {
                            writeln!(writer, "{output_line}")?;
                        }
                        writer.flush()?;
                    }
                }
            }
            Ok(())
        });

        tx.send(StreamResponse::Lines(vec![serde_json::to_string(
            &hello_event(None, options),
        )?]))
        .map_err(|_| DaemonError::StreamClosed)?;

        let active_requests = Arc::new(AtomicUsize::new(0));
        let mut idx = 0usize;
        let mut received_request = false;
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            received_request = true;
            let request_idx = idx;
            idx += 1;

            if active_requests.load(Ordering::SeqCst) >= MAX_PENDING_STREAM_REQUESTS {
                tx.send(StreamResponse::Lines(vec![serde_json::to_string(
                    &pending_request_backpressure_event(request_idx),
                )?]))
                .map_err(|_| DaemonError::StreamClosed)?;
                continue;
            }

            active_requests.fetch_add(1, Ordering::SeqCst);
            let request = trimmed.to_string();
            let options = options.clone();
            let tx = tx.clone();
            let active_requests = active_requests.clone();
            scope.spawn(move || {
                let lines = request_lines(request_idx, &request, &options).unwrap_or_else(|error| {
                    vec![
                        serde_json::to_string(&stream_request_error_event(request_idx, error))
                            .unwrap_or_else(|_| {
                                "{\"schema\":\"opensks.engine-event.v1\",\"event_type\":\"error\",\"message\":\"stream request failed\"}".to_string()
                            }),
                    ]
                });
                let _ = tx.send(StreamResponse::Lines(lines));
                active_requests.fetch_sub(1, Ordering::SeqCst);
            });
        }

        if !received_request {
            tx.send(StreamResponse::Lines(vec![serde_json::to_string(
                &health_event(None, options),
            )?]))
            .map_err(|_| DaemonError::StreamClosed)?;
        }

        drop(tx);
        writer_handle
            .join()
            .map_err(|_| DaemonError::StreamWorkerPanic)??;
        Ok(())
    })
}

fn request_lines(
    idx: usize,
    trimmed: &str,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let request = match serde_json::from_str::<EngineRequest>(trimmed) {
        Ok(request) => request,
        // An unparseable request has no id to correlate a terminal marker to; the
        // client's hard deadline is the only safety net for this defensive path.
        Err(error) => return Ok(vec![serde_json::to_string(&parse_error_event(idx, error))?]),
    };
    let request_id = request.id.clone();

    // Compute the response body lines per request kind, then ALWAYS append an
    // explicit per-request terminal marker (STREAM-001) so the client completes on
    // it rather than on a silence/quiet-window heuristic.
    let body = match request.kind {
        EngineRequestKind::Hello => {
            vec![serde_json::to_string(&hello_event(
                Some(request_id.clone()),
                options,
            ))?]
        }
        EngineRequestKind::Health => {
            vec![serde_json::to_string(&health_event(
                Some(request_id.clone()),
                options,
            ))?]
        }
        EngineRequestKind::SubscribeEvents => match subscription_lines(&request, options) {
            Ok(subscription_lines) => subscription_lines,
            Err(error) => vec![serde_json::to_string(&run_start_error_event(
                Some(request_id.clone()),
                error,
            ))?],
        },
        EngineRequestKind::RunStart => match run_start_lines(&request, options) {
            Ok(run_lines) => run_lines,
            Err(error) => vec![serde_json::to_string(&run_start_error_event(
                Some(request_id.clone()),
                error,
            ))?],
        },
        EngineRequestKind::RunPause
        | EngineRequestKind::RunResume
        | EngineRequestKind::RunCancel
        | EngineRequestKind::RunSteer => match run_control_lines(&request, options) {
            Ok(control_lines) => control_lines,
            Err(error) => vec![serde_json::to_string(&run_start_error_event(
                Some(request_id.clone()),
                error,
            ))?],
        },
        EngineRequestKind::ApprovalRequest
        | EngineRequestKind::ApprovalApprove
        | EngineRequestKind::ApprovalDeny => match approval_lines(&request, options) {
            Ok(approval_lines) => approval_lines,
            Err(error) => vec![serde_json::to_string(&run_start_error_event(
                Some(request_id.clone()),
                error,
            ))?],
        },
        EngineRequestKind::OutboxDispatch => match outbox_dispatch_lines(&request) {
            Ok(outbox_lines) => outbox_lines,
            Err(error) => vec![serde_json::to_string(&run_start_error_event(
                Some(request_id.clone()),
                error,
            ))?],
        },
    };

    let mut lines = body;
    lines.push(serde_json::to_string(&request_completed_event(request_id))?);
    Ok(lines)
}

/// STREAM-001: the explicit per-request terminal marker. Emitted as the final line
/// of every (parseable) request response, correlated by `request_id`, so the client
/// completes the response deterministically instead of inferring completion from a
/// silence/quiet-window heuristic.
fn request_completed_event(request_id: String) -> EngineEvent {
    let event_id = format!("engine-request-completed-{request_id}");
    let mut event = EngineEvent::new(
        event_id,
        Some(request_id),
        EngineEventType::RequestCompleted,
        "request completed",
        timestamp_ms(),
    );
    event.evidence_refs = vec!["daemon:request-completed".to_string()];
    event.redacted = true;
    event
}

fn pending_request_backpressure_event(idx: usize) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-request-backpressure-{idx}"),
        None,
        EngineEventType::Error,
        format!("too many pending stdio requests; max pending is {MAX_PENDING_STREAM_REQUESTS}"),
        timestamp_ms(),
    );
    event.severity = EventSeverity::Warning;
    event.evidence_refs = vec![
        "daemon:pending-request-router".to_string(),
        "daemon:pending-request-backpressure".to_string(),
    ];
    event.redacted = true;
    event
}

fn stream_request_error_event(idx: usize, error: DaemonError) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-request-worker-error-{idx}"),
        None,
        EngineEventType::Error,
        format!("stream request failed: {error}"),
        timestamp_ms(),
    );
    event.severity = EventSeverity::Error;
    event.evidence_refs = vec!["daemon:pending-request-worker-error".to_string()];
    event.redacted = true;
    event
}

fn hello_event(request_id: Option<String>, options: &DaemonOptions) -> EngineEvent {
    let mut event = EngineEvent::new(
        "engine-hello",
        request_id,
        EngineEventType::EngineHello,
        "OpenSKS engine daemon ready",
        timestamp_ms(),
    );
    event.evidence_refs = vec!["contracts:opensks.engine-event.v1".to_string()];
    event.redacted = true;
    if options.workspace.as_os_str().is_empty() {
        event.severity = EventSeverity::Warning;
    }
    event
}

fn health_event(request_id: Option<String>, _options: &DaemonOptions) -> EngineEvent {
    let mut event = EngineEvent::new(
        "engine-health",
        request_id,
        EngineEventType::EngineHealth,
        "health ok",
        timestamp_ms(),
    );
    event.evidence_refs = vec!["daemon:stdio-health".to_string()];
    event.redacted = true;
    event
}

fn subscription_event(
    request_id: Option<String>,
    run_id: &str,
    replayed_count: usize,
    since_sequence: u64,
) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-subscribe-events-{run_id}"),
        request_id,
        EngineEventType::ExecutionEvent,
        format!("event stream replayed {replayed_count} events since sequence {since_sequence}"),
        timestamp_ms(),
    );
    event.evidence_refs = vec![
        "daemon:subscription-accepted".to_string(),
        "event-store:replay-since".to_string(),
    ];
    event.redacted = true;
    event
}

fn subscription_tail_complete_event(
    request_id: Option<String>,
    run_id: &str,
    cursor: u64,
    emitted_count: usize,
    tail_ms: u64,
    poll_interval_ms: u64,
) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-subscribe-tail-complete-{run_id}"),
        request_id,
        EngineEventType::ExecutionEvent,
        format!(
            "event stream tail completed at sequence {cursor} after {tail_ms}ms with {emitted_count} new events"
        ),
        timestamp_ms(),
    );
    event.evidence_refs = vec![
        "daemon:subscription-tail-complete".to_string(),
        "event-store:poll-tail".to_string(),
        format!("daemon:poll-interval-ms:{poll_interval_ms}"),
    ];
    event.redacted = true;
    event
}

fn subscription_error_event(request_id: &str, run_id: &str) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-subscribe-events-error-{run_id}"),
        Some(request_id.to_string()),
        EngineEventType::Error,
        "event replay failed",
        timestamp_ms(),
    );
    event.severity = EventSeverity::Error;
    event.redacted = true;
    event.evidence_refs = vec!["daemon:subscription-error".to_string()];
    event
}

fn subscription_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let Some(run_id) = request.params.run_id.as_deref() else {
        return Ok(vec![serde_json::to_string(&missing_param_event(
            &request.id,
            "run_id",
        ))?]);
    };
    let since_sequence = request.params.since_sequence.unwrap_or(0);
    let tail_ms = request
        .params
        .tail_ms
        .unwrap_or(0)
        .min(SUBSCRIPTION_TAIL_MAX_MS);
    let poll_interval_ms = request
        .params
        .poll_interval_ms
        .unwrap_or(SUBSCRIPTION_TAIL_DEFAULT_POLL_MS)
        .clamp(SUBSCRIPTION_TAIL_MIN_POLL_MS, SUBSCRIPTION_TAIL_MAX_POLL_MS);
    let mut lines = Vec::new();
    let Ok(store) = opensks_event_store::EventStore::open_workspace(&options.workspace) else {
        lines.push(serde_json::to_string(&subscription_error_event(
            &request.id,
            run_id,
        ))?);
        return Ok(lines);
    };

    let Ok(events) = store.replay_since(run_id, since_sequence) else {
        lines.push(serde_json::to_string(&subscription_error_event(
            &request.id,
            run_id,
        ))?);
        return Ok(lines);
    };

    lines.push(serde_json::to_string(&subscription_event(
        Some(request.id.clone()),
        run_id,
        events.len(),
        since_sequence,
    ))?);
    let mut cursor = since_sequence;
    for event in events {
        cursor = cursor.max(event.sequence);
        lines.push(serde_json::to_string(&event)?);
    }

    if tail_ms > 0 {
        let deadline = Instant::now() + Duration::from_millis(tail_ms);
        let mut emitted_count = 0usize;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(Duration::from_millis(poll_interval_ms).min(remaining));
            let Ok(events) = store.replay_since(run_id, cursor) else {
                lines.push(serde_json::to_string(&subscription_error_event(
                    &request.id,
                    run_id,
                ))?);
                return Ok(lines);
            };
            emitted_count += events.len();
            for event in events {
                cursor = cursor.max(event.sequence);
                lines.push(serde_json::to_string(&event)?);
            }
        }
        lines.push(serde_json::to_string(&subscription_tail_complete_event(
            Some(request.id.clone()),
            run_id,
            cursor,
            emitted_count,
            tail_ms,
            poll_interval_ms,
        ))?);
    }

    Ok(lines)
}

fn run_start_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let pipeline_id = request
        .params
        .pipeline_id
        .as_deref()
        .unwrap_or("single-model-safe");
    let objective = request
        .params
        .objective
        .as_deref()
        .unwrap_or("OpenSKS daemon run.start smoke");
    let run_id = request
        .params
        .run_id
        .clone()
        .unwrap_or_else(|| format!("run-{}-{}", request.id, timestamp_ms()));
    let result = if let Some(graph_path) = request.params.graph_path.as_deref() {
        match load_workspace_graph(&options.workspace, graph_path) {
            Ok(graph) => opensks_engine::run_graph_with_event_stream(
                &options.workspace,
                &run_id,
                &graph,
                objective,
                "daemon:graph-path-run-start-request",
                "workspace_graph_path",
            ),
            Err(reason) => {
                let event = graph_path_error_event(&request.id, &run_id, reason);
                return Ok(vec![serde_json::to_string(&event)?]);
            }
        }
    } else {
        opensks_engine::run_template_with_event_stream(
            &options.workspace,
            &run_id,
            pipeline_id,
            objective,
        )
    };
    let mut lines = Vec::new();
    match result {
        Ok(result) => {
            let mut accepted = EngineEvent::new(
                format!("engine-run-start-{}", result.run_id),
                Some(request.id.clone()),
                EngineEventType::ExecutionEvent,
                format!(
                    "run.start accepted for {} with {} work items",
                    result.template_id,
                    result.snapshot.work_items.len()
                ),
                timestamp_ms(),
            );
            accepted.evidence_refs = vec![
                "daemon:run-start".to_string(),
                "engine:event-store-replay".to_string(),
            ];
            accepted.redacted = true;
            lines.push(serde_json::to_string(&accepted)?);
            for event in result.events {
                lines.push(serde_json::to_string(&event)?);
            }
        }
        Err(error) => {
            let mut event = EngineEvent::new(
                format!("engine-run-start-error-{run_id}"),
                Some(request.id.clone()),
                EngineEventType::Error,
                format!("run.start failed: {error}"),
                timestamp_ms(),
            );
            event.severity = EventSeverity::Error;
            event.redacted = true;
            event.evidence_refs = vec!["daemon:run-start-error".to_string()];
            lines.push(serde_json::to_string(&event)?);
        }
    }
    Ok(lines)
}

fn load_workspace_graph(workspace: &Path, requested: &str) -> Result<PipelineGraph, &'static str> {
    let relative = Path::new(requested);
    if requested.trim().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("graph_path must be a workspace-relative file path");
    }

    let workspace = workspace
        .canonicalize()
        .map_err(|_| "workspace is not readable")?;
    let graph_path = workspace.join(relative);
    let graph_path = graph_path
        .canonicalize()
        .map_err(|_| "graph file is not readable")?;
    if !graph_path.starts_with(&workspace) {
        return Err("graph_path escapes the workspace");
    }
    let bytes = std::fs::read(&graph_path).map_err(|_| "graph file is not readable")?;
    serde_json::from_slice(&bytes).map_err(|_| "graph file is not a PipelineGraph")
}

fn graph_path_error_event(request_id: &str, run_id: &str, reason: &str) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-run-start-graph-path-error-{run_id}"),
        Some(request_id.to_string()),
        EngineEventType::Error,
        format!("run.start failed: invalid graph_path: {reason}"),
        timestamp_ms(),
    );
    event.severity = EventSeverity::Error;
    event.redacted = true;
    event.evidence_refs = vec!["daemon:graph-path-error".to_string()];
    event
}

fn run_start_error_event(request_id: Option<String>, error: DaemonError) -> EngineEvent {
    let mut event = EngineEvent::new(
        "engine-run-start-error",
        request_id,
        EngineEventType::Error,
        format!("run.start failed: {error}"),
        timestamp_ms(),
    );
    event.severity = EventSeverity::Error;
    event.redacted = true;
    event
}

fn run_control_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let Some(run_id) = request.params.run_id.as_deref() else {
        let mut event = EngineEvent::new(
            "engine-run-control-missing-run-id",
            Some(request.id.clone()),
            EngineEventType::Error,
            "run control request requires params.run_id",
            timestamp_ms(),
        );
        event.severity = EventSeverity::Error;
        event.redacted = true;
        return Ok(vec![serde_json::to_string(&event)?]);
    };
    let (kind, message, reason_code) = match request.kind {
        EngineRequestKind::RunPause => (
            EventKind::RunPaused,
            request
                .params
                .message
                .as_deref()
                .unwrap_or("run pause requested"),
            request
                .params
                .reason_code
                .as_deref()
                .unwrap_or("paused_by_user"),
        ),
        EngineRequestKind::RunResume => (
            EventKind::RunResumed,
            request
                .params
                .message
                .as_deref()
                .unwrap_or("run resume requested"),
            request
                .params
                .reason_code
                .as_deref()
                .unwrap_or("resumed_by_user"),
        ),
        EngineRequestKind::RunCancel => (
            EventKind::RunCancelled,
            request
                .params
                .message
                .as_deref()
                .unwrap_or("run cancel requested"),
            request
                .params
                .reason_code
                .as_deref()
                .unwrap_or("cancelled_by_user"),
        ),
        EngineRequestKind::RunSteer => (
            EventKind::SteeringRequested,
            request
                .params
                .message
                .as_deref()
                .unwrap_or("steering requested"),
            request
                .params
                .reason_code
                .as_deref()
                .unwrap_or("user_steering"),
        ),
        _ => (
            EventKind::Unknown,
            "invalid run control request",
            "invalid_control_request",
        ),
    };
    let result = opensks_engine::append_run_control_event(
        &options.workspace,
        run_id,
        kind,
        request.params.target_id.as_deref(),
        message,
        reason_code,
    );
    let mut lines = Vec::new();
    match result {
        Ok(result) => {
            let mut accepted = EngineEvent::new(
                format!("engine-run-control-{}-{}", run_id, result.event.sequence),
                Some(request.id.clone()),
                EngineEventType::ExecutionEvent,
                format!("run control accepted: {}", result.event.kind.as_str()),
                timestamp_ms(),
            );
            accepted.evidence_refs = vec!["daemon:run-control".to_string()];
            accepted.redacted = true;
            lines.push(serde_json::to_string(&accepted)?);
            lines.push(serde_json::to_string(&result.event)?);
        }
        Err(error) => {
            let mut event = EngineEvent::new(
                format!("engine-run-control-error-{run_id}"),
                Some(request.id.clone()),
                EngineEventType::Error,
                format!("run control failed: {error}"),
                timestamp_ms(),
            );
            event.severity = EventSeverity::Error;
            event.redacted = true;
            event.evidence_refs = vec!["daemon:run-control-error".to_string()];
            lines.push(serde_json::to_string(&event)?);
        }
    }
    Ok(lines)
}

fn approval_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let Some(run_id) = request.params.run_id.as_deref() else {
        return Ok(vec![serde_json::to_string(&missing_param_event(
            &request.id,
            "run_id",
        ))?]);
    };
    let Some(approval_id) = request.params.approval_id.as_deref() else {
        return Ok(vec![serde_json::to_string(&missing_param_event(
            &request.id,
            "approval_id",
        ))?]);
    };
    let (kind, state, default_scope, default_message, default_reason) = match request.kind {
        EngineRequestKind::ApprovalRequest => (
            EventKind::ApprovalRequested,
            "pending",
            "project",
            "approval requested",
            "approval_required",
        ),
        EngineRequestKind::ApprovalApprove => (
            EventKind::ApprovalApproved,
            "approved",
            "project",
            "approval approved",
            "approved_by_user",
        ),
        EngineRequestKind::ApprovalDeny => (
            EventKind::ApprovalDenied,
            "denied",
            "project",
            "approval denied",
            "denied_by_user",
        ),
        _ => (
            EventKind::Unknown,
            "invalid",
            "project",
            "invalid approval request",
            "invalid_approval_request",
        ),
    };
    let scope = request.params.scope.as_deref().unwrap_or(default_scope);
    let message = request.params.message.as_deref().unwrap_or(default_message);
    let reason_code = request
        .params
        .reason_code
        .as_deref()
        .unwrap_or(default_reason);
    let result = opensks_engine::append_approval_event(
        &options.workspace,
        opensks_engine::EngineApprovalEventInput {
            run_id,
            kind,
            approval_id,
            scope,
            state,
            message,
            reason_code,
        },
    );
    let mut lines = Vec::new();
    match result {
        Ok(result) => {
            let mut accepted = EngineEvent::new(
                format!("engine-approval-{}-{}", run_id, result.event.sequence),
                Some(request.id.clone()),
                EngineEventType::ExecutionEvent,
                format!("approval event accepted: {}", result.event.kind.as_str()),
                timestamp_ms(),
            );
            accepted.evidence_refs = vec!["daemon:approval".to_string()];
            accepted.redacted = true;
            lines.push(serde_json::to_string(&accepted)?);
            lines.push(serde_json::to_string(&result.event)?);
        }
        Err(error) => {
            let mut event = EngineEvent::new(
                format!("engine-approval-error-{run_id}"),
                Some(request.id.clone()),
                EngineEventType::Error,
                format!("approval failed: {error}"),
                timestamp_ms(),
            );
            event.severity = EventSeverity::Error;
            event.redacted = true;
            event.evidence_refs = vec!["daemon:approval-error".to_string()];
            lines.push(serde_json::to_string(&event)?);
        }
    }
    Ok(lines)
}

fn outbox_dispatch_lines(request: &EngineRequest) -> Result<Vec<String>, DaemonError> {
    let target = request.params.target_id.as_deref().unwrap_or("main");
    let mut outbox = opensks_git::Outbox::new();
    let item = match outbox.enqueue_push(target, request.params.approval_id.is_some()) {
        Ok(item) => item,
        Err(error) => {
            let mut event = EngineEvent::new(
                format!("engine-outbox-dispatch-error-{target}"),
                Some(request.id.clone()),
                EngineEventType::Error,
                format!("outbox dispatch enqueue failed: {error}"),
                timestamp_ms(),
            );
            event.severity = EventSeverity::Error;
            event.redacted = true;
            event.evidence_refs = vec!["daemon:outbox-dispatch-error".to_string()];
            return Ok(vec![serde_json::to_string(&event)?]);
        }
    };
    let approvals = if request
        .params
        .approval_id
        .as_ref()
        .zip(item.approval_id.as_ref())
        .is_some_and(|(requested, required)| requested == required)
    {
        vec![opensks_git::OutboxApproval {
            approval_id: item
                .approval_id
                .clone()
                .unwrap_or_else(|| format!("approval-push-{target}")),
            scope: "git_push".to_string(),
            target: target.to_string(),
            approved: true,
        }]
    } else {
        Vec::new()
    };
    let report = outbox
        .dispatch_next(&approvals, |_| Ok(()))
        .unwrap_or_else(|error| opensks_contracts::OutboxDispatchReport {
            schema: opensks_contracts::OUTBOX_DISPATCH_REPORT_SCHEMA.to_string(),
            item_id: format!("push-{target}"),
            action: opensks_contracts::OutboxAction::Push,
            target: target.to_string(),
            approval_id: item.approval_id.clone(),
            executed: false,
            state: "blocked".to_string(),
            reason_code: format!("dispatch_error:{error}"),
            attempt_count: 0,
            evidence_refs: vec!["daemon:outbox-dispatch-error".to_string()],
        });
    let mut accepted = EngineEvent::new(
        format!("engine-outbox-dispatch-{target}"),
        Some(request.id.clone()),
        EngineEventType::ExecutionEvent,
        format!(
            "outbox dispatch {} for {target}: {}",
            report.state, report.reason_code
        ),
        timestamp_ms(),
    );
    accepted.evidence_refs = vec!["daemon:outbox-dispatch".to_string()];
    accepted.redacted = true;
    Ok(vec![
        serde_json::to_string(&accepted)?,
        serde_json::to_string(&report)?,
    ])
}

fn missing_param_event(request_id: &str, param: &str) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-missing-param-{param}"),
        Some(request_id.to_string()),
        EngineEventType::Error,
        format!("request requires params.{param}"),
        timestamp_ms(),
    );
    event.severity = EventSeverity::Error;
    event.redacted = true;
    event
}

fn parse_error_event(idx: usize, error: serde_json::Error) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-error-{idx}"),
        None,
        EngineEventType::Error,
        format!("invalid request JSON: {error}"),
        timestamp_ms(),
    );
    event.severity = EventSeverity::Error;
    event.redacted = true;
    event
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensks_contracts::EngineRequest;
    use std::io::Cursor;

    #[test]
    fn empty_stdio_emits_hello_and_health_without_workspace_path() {
        let output = run_stdio(
            "",
            &DaemonOptions {
                workspace: PathBuf::from("/tmp/secret-workspace"),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"event_type\":\"engine_hello\""));
        assert!(output.contains("\"event_type\":\"engine_health\""));
        assert!(!output.contains("/tmp/secret-workspace"));
    }

    #[test]
    fn streaming_empty_stdio_emits_hello_and_health_without_workspace_path() {
        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(Vec::<u8>::new()),
            &mut output,
            &DaemonOptions {
                workspace: PathBuf::from("/tmp/secret-workspace"),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("\"event_type\":\"engine_hello\""));
        assert!(output.contains("\"event_type\":\"engine_health\""));
        assert!(!output.contains("/tmp/secret-workspace"));
    }

    #[test]
    fn streaming_stdio_handles_multiple_requests_in_order() {
        let input = [
            serde_json::to_string(&EngineRequest::health("req-health-1")).expect("request"),
            serde_json::to_string(&EngineRequest::health("req-health-2")).expect("request"),
        ]
        .join("\n");
        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(input + "\n"),
            &mut output,
            &DaemonOptions {
                workspace: PathBuf::from("."),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("\"request_id\":\"req-health-1\""));
        assert!(output.contains("\"request_id\":\"req-health-2\""));
    }

    #[test]
    fn request_response_ends_with_an_explicit_terminal_marker() {
        // STREAM-001: every request response ends with an explicit, request_id-
        // correlated terminal marker so the client completes on it — never on a
        // silence/quiet-window heuristic.
        let input = serde_json::to_string(&EngineRequest::health("req-term-1")).expect("request");
        let output = run_stdio(
            &input,
            &DaemonOptions {
                workspace: PathBuf::from("."),
            },
        )
        .expect("stdio");

        assert!(output.contains("\"event_type\":\"request_completed\""));
        assert!(output.contains("\"daemon:request-completed\""));
        // Exactly one terminal marker for the one request (the startup hello/health
        // banner is not a request response and carries none).
        assert_eq!(
            output
                .matches("\"event_type\":\"request_completed\"")
                .count(),
            1
        );
        // The terminal marker carries the request id AND is the final line, so the
        // client can correlate it and stop reading deterministically.
        let last = output
            .lines()
            .rfind(|line| !line.trim().is_empty())
            .expect("at least one output line");
        assert!(
            last.contains("\"event_type\":\"request_completed\""),
            "terminal marker must be the final line, got: {last}"
        );
        assert!(
            last.contains("\"request_id\":\"req-term-1\""),
            "terminal marker must carry the request id, got: {last}"
        );
    }

    #[test]
    fn streaming_stdio_routes_health_without_waiting_for_tail_subscribe_completion() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-daemon-stream-router-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut subscribe =
            EngineRequest::subscribe_events("req-tail-slow", "run-daemon-stream-router", Some(0));
        subscribe.params.tail_ms = Some(250);
        subscribe.params.poll_interval_ms = Some(10);
        let input = [
            serde_json::to_string(&subscribe).expect("subscribe"),
            serde_json::to_string(&EngineRequest::health("req-health-behind-tail"))
                .expect("health"),
        ]
        .join("\n");
        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(input + "\n"),
            &mut output,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");
        let health_pos = output
            .find("\"request_id\":\"req-health-behind-tail\"")
            .expect("health response");
        let tail_complete_pos = output
            .find("\"daemon:subscription-tail-complete\"")
            .expect("tail completion");
        assert!(
            health_pos < tail_complete_pos,
            "health response should not be blocked behind tail completion:\n{output}"
        );
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn health_request_is_correlated() {
        let input = serde_json::to_string(&EngineRequest::health("req-health")).expect("request");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: PathBuf::from("."),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"request_id\":\"req-health\""));
        assert!(output.contains("health ok"));
    }

    #[test]
    fn run_start_request_emits_execution_event_envelopes() {
        let workspace =
            std::env::temp_dir().join(format!("opensks-daemon-run-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut request =
            EngineRequest::run_start("req-run", "single-model-safe", "run daemon graph");
        request.params.run_id = Some("run-daemon-test".to_string());
        let input = serde_json::to_string(&request).expect("request");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"event_type\":\"engine_hello\""));
        assert!(output.contains("\"event_type\":\"execution_event\""));
        assert!(output.contains("\"schema\":\"opensks.execution-event-envelope.v1\""));
        assert!(output.contains("\"kind\":\"run_started\""));
        assert!(output.contains("\"kind\":\"work_item_running\""));
        assert!(output.contains("\"kind\":\"snapshot_written\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn run_start_request_loads_workspace_graph_path() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-daemon-run-graph-path-{}",
            std::process::id()
        ));
        let graph_dir = workspace.join(".opensks").join("pipelines").join("editor");
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&graph_dir).expect("graph dir");
        std::fs::write(
            graph_dir.join("current.graph.json"),
            serde_json::to_string(&opensks_graph::single_model_safe_template()).expect("graph"),
        )
        .expect("write graph");
        let mut request =
            EngineRequest::run_start("req-run-graph", "editor-draft", "run saved graph");
        request.params.run_id = Some("run-daemon-graph-path".to_string());
        request.params.graph_path =
            Some(".opensks/pipelines/editor/current.graph.json".to_string());
        let input = serde_json::to_string(&request).expect("request");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"event_type\":\"execution_event\""));
        assert!(output.contains("\"kind\":\"run_started\""));
        assert!(output.contains("\"graph_source\":\"workspace_graph_path\""));
        assert!(output.contains("\"daemon:graph-path-run-start-request\""));
        assert!(output.contains("\"kind\":\"snapshot_written\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn run_start_request_rejects_workspace_escape_graph_path() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-daemon-run-graph-path-escape-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut request =
            EngineRequest::run_start("req-run-graph-escape", "editor-draft", "run saved graph");
        request.params.run_id = Some("run-daemon-graph-path-escape".to_string());
        request.params.graph_path = Some("../outside.graph.json".to_string());
        let input = serde_json::to_string(&request).expect("request");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"event_type\":\"error\""));
        assert!(output.contains("invalid graph_path"));
        assert!(output.contains("workspace-relative"));
        assert!(output.contains("\"daemon:graph-path-error\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn run_start_request_reports_invalid_graph_path_json_without_panicking() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-daemon-run-graph-path-invalid-{}",
            std::process::id()
        ));
        let graph_dir = workspace.join(".opensks").join("pipelines").join("editor");
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&graph_dir).expect("graph dir");
        std::fs::write(
            graph_dir.join("current.graph.json"),
            r#"{"schema":"opensks.graph-editor-document.v1","id":"draft"}"#,
        )
        .expect("write graph");
        let mut request =
            EngineRequest::run_start("req-run-graph-invalid", "editor-draft", "run saved graph");
        request.params.run_id = Some("run-daemon-graph-path-invalid".to_string());
        request.params.graph_path =
            Some(".opensks/pipelines/editor/current.graph.json".to_string());
        let input = serde_json::to_string(&request).expect("request");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"event_type\":\"error\""));
        assert!(output.contains("not a PipelineGraph"));
        assert!(output.contains("\"daemon:graph-path-error\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn subscribe_events_replays_committed_events_for_reconnect() {
        let workspace =
            std::env::temp_dir().join(format!("opensks-daemon-subscribe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut start =
            EngineRequest::run_start("req-run", "single-model-safe", "run daemon replay");
        start.params.run_id = Some("run-daemon-replay".to_string());
        let subscribe =
            EngineRequest::subscribe_events("req-subscribe", "run-daemon-replay", Some(1));
        let input = [
            serde_json::to_string(&start).expect("start"),
            serde_json::to_string(&subscribe).expect("subscribe"),
        ]
        .join("\n");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"request_id\":\"req-subscribe\""));
        assert!(output.contains("event stream replayed"));
        assert!(output.contains("\"event-store:replay-since\""));
        assert!(output.contains("\"kind\":\"snapshot_written\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn subscribe_events_tail_completes_without_workspace_path_leak() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-daemon-subscribe-tail-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut start = EngineRequest::run_start("req-run", "single-model-safe", "run daemon tail");
        start.params.run_id = Some("run-daemon-tail".to_string());
        let mut subscribe =
            EngineRequest::subscribe_events("req-tail", "run-daemon-tail", Some(999));
        subscribe.params.tail_ms = Some(1);
        subscribe.params.poll_interval_ms = Some(1);
        let input = [
            serde_json::to_string(&start).expect("start"),
            serde_json::to_string(&subscribe).expect("subscribe"),
        ]
        .join("\n");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"request_id\":\"req-tail\""));
        assert!(output.contains("event stream replayed 0 events since sequence 999"));
        assert!(output.contains("event stream tail completed"));
        assert!(output.contains("\"daemon:subscription-tail-complete\""));
        assert!(output.contains("\"event-store:poll-tail\""));
        assert!(output.contains("\"daemon:poll-interval-ms:10\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn run_control_request_appends_control_event() {
        let workspace =
            std::env::temp_dir().join(format!("opensks-daemon-run-control-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut start =
            EngineRequest::run_start("req-run", "single-model-safe", "run daemon graph");
        start.params.run_id = Some("run-daemon-control".to_string());
        let cancel = EngineRequest::run_cancel("req-cancel", "run-daemon-control");
        let steer = EngineRequest::run_steer(
            "req-steer",
            "run-daemon-control",
            "work-template-delegate",
            "focus delegate on tests",
        );
        let input = [
            serde_json::to_string(&start).expect("start"),
            serde_json::to_string(&cancel).expect("cancel"),
            serde_json::to_string(&steer).expect("steer"),
        ]
        .join("\n");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"kind\":\"run_cancelled\""));
        assert!(output.contains("\"kind\":\"steering_requested\""));
        assert!(output.contains("\"work_item_id\":\"work-template-delegate\""));
        assert!(output.contains("\"daemon:run-control-request\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn approval_request_and_decision_append_approval_events() {
        let workspace =
            std::env::temp_dir().join(format!("opensks-daemon-approval-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut start =
            EngineRequest::run_start("req-run", "single-model-safe", "run daemon approvals");
        start.params.run_id = Some("run-daemon-approval".to_string());
        let request = EngineRequest::approval_request(
            "req-approval",
            "run-daemon-approval",
            "approval-1",
            "git_push",
            "Approve Git push",
        );
        let approve = EngineRequest::approval_decision(
            "req-approve",
            "run-daemon-approval",
            "approval-1",
            true,
        );
        let input = [
            serde_json::to_string(&start).expect("start"),
            serde_json::to_string(&request).expect("approval request"),
            serde_json::to_string(&approve).expect("approval approve"),
        ]
        .join("\n");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"kind\":\"approval_requested\""));
        assert!(output.contains("\"kind\":\"approval_approved\""));
        assert!(output.contains("\"approval_id\":\"approval-1\""));
        assert!(output.contains("\"state\":\"approved\""));
        assert!(output.contains("\"daemon:approval-request\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn outbox_dispatch_request_blocks_without_approval_and_reports_dry_run_approval() {
        let blocked = EngineRequest::outbox_dispatch("req-outbox-blocked", "main", None);
        let approved = EngineRequest::outbox_dispatch(
            "req-outbox-approved",
            "main",
            Some("approval-push-main".to_string()),
        );
        let input = [
            serde_json::to_string(&blocked).expect("blocked outbox"),
            serde_json::to_string(&approved).expect("approved outbox"),
        ]
        .join("\n");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: PathBuf::from("."),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"schema\":\"opensks.outbox-dispatch-report.v1\""));
        assert!(output.contains("\"state\":\"awaiting_approval\""));
        assert!(output.contains("\"reason_code\":\"approval_required\""));
        assert!(output.contains("\"state\":\"executed\""));
        assert!(output.contains("\"reason_code\":\"executed_after_approval\""));
        assert!(output.contains("\"daemon:outbox-dispatch\""));
    }
}
