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
    #[error("conversation repository error: {0}")]
    Conversation(#[from] opensks_conversation::ConversationError),
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
        EngineRequestKind::ConversationTurnStart => {
            match conversation_turn_start_lines(&request, options) {
                Ok(turn_lines) => turn_lines,
                Err(error) => vec![serde_json::to_string(&run_start_error_event(
                    Some(request_id.clone()),
                    error,
                ))?],
            }
        }
        EngineRequestKind::ConversationSupervisorTick => {
            match conversation_supervisor_tick_lines(&request, options) {
                Ok(tick_lines) => tick_lines,
                Err(error) => vec![serde_json::to_string(&run_start_error_event(
                    Some(request_id.clone()),
                    error,
                ))?],
            }
        }
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

fn conversation_turn_start_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let Some(turn_request) = request.params.conversation_turn_start.as_ref() else {
        return Ok(vec![serde_json::to_string(&missing_param_event(
            &request.id,
            "conversation_turn_start",
        ))?]);
    };
    if turn_request.request_id != request.id {
        let mut event = EngineEvent::new(
            format!(
                "engine-conversation-turn-start-request-id-mismatch-{}",
                request.id
            ),
            Some(request.id.clone()),
            EngineEventType::Error,
            "conversation turn start request_id must match engine request id",
            timestamp_ms(),
        );
        event.severity = EventSeverity::Error;
        event.evidence_refs = vec!["daemon:conversation-turn-start-request-id".to_string()];
        event.redacted = true;
        return Ok(vec![serde_json::to_string(&event)?]);
    }

    let repo =
        match opensks_conversation::ConversationRepository::open_workspace(&options.workspace) {
            Ok(repo) => repo,
            Err(error) => {
                let mut event = EngineEvent::new(
                    format!("engine-conversation-turn-start-open-error-{}", request.id),
                    Some(request.id.clone()),
                    EngineEventType::Error,
                    format!("conversation turn start failed: {error}"),
                    timestamp_ms(),
                );
                event.severity = EventSeverity::Error;
                event.evidence_refs = vec!["daemon:conversation-turn-start-open-error".to_string()];
                event.redacted = true;
                return Ok(vec![serde_json::to_string(&event)?]);
            }
        };
    match repo.accept_conversation_turn(turn_request, timestamp_ms()) {
        Ok(accepted) => match record_model_routing_decision(&repo, &accepted.turn_id) {
            Ok(()) => Ok(vec![serde_json::to_string(&accepted)?]),
            Err(error) => {
                let mut event = EngineEvent::new(
                    format!(
                        "engine-conversation-turn-start-routing-error-{}",
                        request.id
                    ),
                    Some(request.id.clone()),
                    EngineEventType::Error,
                    format!("conversation turn start routing receipt failed: {error}"),
                    timestamp_ms(),
                );
                event.severity = EventSeverity::Error;
                event.evidence_refs =
                    vec!["daemon:conversation-turn-start-routing-error".to_string()];
                event.redacted = true;
                Ok(vec![serde_json::to_string(&event)?])
            }
        },
        Err(error) => {
            let mut event = EngineEvent::new(
                format!("engine-conversation-turn-start-error-{}", request.id),
                Some(request.id.clone()),
                EngineEventType::Error,
                format!("conversation turn start failed: {error}"),
                timestamp_ms(),
            );
            event.severity = EventSeverity::Error;
            event.evidence_refs = vec!["daemon:conversation-turn-start-error".to_string()];
            event.redacted = true;
            Ok(vec![serde_json::to_string(&event)?])
        }
    }
}

fn record_model_routing_decision(
    repo: &opensks_conversation::ConversationRepository,
    turn_id: &str,
) -> Result<(), DaemonError> {
    let Some(settings_json) = repo.turn_effective_settings_json(turn_id)? else {
        return Ok(());
    };
    let settings: opensks_contracts::ConversationTurnSettings =
        serde_json::from_str(&settings_json)?;
    let decision = opensks_provider::routing_decision_from_turn_settings(
        format!("route-{turn_id}"),
        &settings,
    );
    let decision_json = serde_json::to_string(&decision)?;
    repo.set_turn_model_routing_decision(turn_id, &decision_json, timestamp_ms())?;
    Ok(())
}

fn conversation_supervisor_tick_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let supervisor_id = request
        .params
        .supervisor_id
        .as_deref()
        .unwrap_or("daemon-turn-supervisor");
    let lease_ttl_ms = request.params.lease_ttl_ms.unwrap_or(30_000);
    let repo = opensks_conversation::ConversationRepository::open_workspace(&options.workspace)?;
    let now_ms = timestamp_ms();
    let recovered = repo.recover_expired_turn_supervisor_leases(now_ms)?;
    let claimed = repo.claim_next_queued_turn(supervisor_id, lease_ttl_ms, now_ms)?;
    let (claimed_json, executed_json) = match claimed {
        Some(lease) => {
            let executed = execute_claimed_conversation_turn(&repo, &options.workspace, &lease);
            let claimed_json = serde_json::json!({
                "turn_id": lease.turn_id,
                "run_id": lease.run_id,
                "project_id": lease.project_id,
                "conversation_id": lease.conversation_id,
                "assistant_message_id": lease.assistant_message_id,
                "lease_owner": lease.lease_owner,
                "lease_expires_at_ms": lease.lease_expires_at_ms,
                "has_model_routing_decision": lease.model_routing_decision_json.is_some(),
            });
            (Some(claimed_json), Some(executed))
        }
        None => (None, None),
    };
    Ok(vec![serde_json::to_string(&serde_json::json!({
        "schema": "opensks.turn-supervisor-tick.v1",
        "request_id": request.id,
        "supervisor_id": supervisor_id,
        "recovered_expired_leases": recovered,
        "claimed": claimed_json,
        "executed": executed_json,
    }))?])
}

fn execute_claimed_conversation_turn(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
) -> serde_json::Value {
    match execute_claimed_conversation_turn_inner(repo, workspace, lease) {
        Ok(value) => value,
        Err(error) => {
            let now_ms = timestamp_ms();
            let _ = repo.set_message_content(
                &lease.assistant_message_id,
                &format!("TurnSupervisor failed: {error}"),
                opensks_contracts::MessageState::Failed,
                now_ms,
            );
            let _ = repo.finish_turn_supervisor_lease(
                &lease.turn_id,
                &lease.run_id,
                "failed",
                0,
                "failed",
                now_ms,
            );
            serde_json::json!({
                "status": "failed",
                "run_state": "failed",
                "error": error,
                "last_event_sequence": 0,
            })
        }
    }
}

fn execute_claimed_conversation_turn_inner(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
) -> Result<serde_json::Value, String> {
    let now_ms = timestamp_ms();
    let prompt = repo
        .turn_user_message_text(&lease.turn_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "accepted turn has no durable user prompt".to_string())?;
    let request = opensks_adapter::AgentRunRequest {
        workspace: workspace.to_path_buf(),
        project_id: lease.project_id.clone(),
        conversation_id: lease.conversation_id.clone(),
        turn_id: lease.turn_id.clone(),
        run_id: lease.run_id.clone(),
        stream_id: format!("stream-{}", lease.turn_id),
        now_ms,
        prompt: prompt.clone(),
    };
    let sink = DaemonAgentEventSink::open(workspace).map_err(|error| error.to_string())?;
    sink.emit_run_started(&request)
        .map_err(|error| error.to_string())?;

    let selected_model_id = lease
        .model_routing_decision_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<opensks_contracts::RoutingDecision>(raw).ok())
        .and_then(|decision| decision.selected_model_id);
    let model = selected_model_id.map(opensks_adapter::OpenRouterAdapter::new);
    let explicit_local_test = opensks_adapter::LocalTestInstruction::from_prompt(&prompt).is_some();
    let outcome = if let Some(model) = model.filter(opensks_adapter::OpenRouterAdapter::is_configured) {
        let completer = opensks_adapter::NativeHttpChatCompleter::new(model.clone());
        let mut driver = opensks_adapter::OpenRouterToolDriver::new(
            model.model.clone(),
            model.max_tokens,
            completer,
            "You are a coding agent. Use workspace tools for file changes; final text alone must not claim files changed.",
            &prompt,
        );
        opensks_adapter::run_agentic_loop(
            &request,
            &mut driver,
            &opensks_adapter::AgenticConfig::default(),
            &sink,
        )
    } else if explicit_local_test {
        run_explicit_local_test_turn(&request, &sink)
    } else {
        opensks_adapter::AgentEventSink::emit(&sink, setup_required_agent_event(&request));
        Ok(opensks_adapter::AgentRunOutcome {
            assistant_text: "Needs setup — connect at least one code-capable model.".to_string(),
            patches: vec![],
            apply_results: vec![],
            final_state: opensks_contracts::projection::RunProjectionState::Failed,
        })
    }
    .map_err(|error| error.to_string())?;
    let last_event_sequence = sink
        .finish(&lease.run_id)
        .map_err(|error| error.to_string())?;
    let (run_state, assistant_state, terminal_kind) = match outcome.final_state {
        opensks_contracts::projection::RunProjectionState::Completed => (
            "completed",
            opensks_contracts::MessageState::Complete,
            "completed",
        ),
        opensks_contracts::projection::RunProjectionState::Failed => {
            ("failed", opensks_contracts::MessageState::Failed, "failed")
        }
        opensks_contracts::projection::RunProjectionState::Cancelled => (
            "cancelled",
            opensks_contracts::MessageState::Cancelled,
            "cancelled",
        ),
        opensks_contracts::projection::RunProjectionState::Paused => (
            "paused",
            opensks_contracts::MessageState::Streaming,
            "paused",
        ),
        opensks_contracts::projection::RunProjectionState::Queued
        | opensks_contracts::projection::RunProjectionState::Running => (
            "running",
            opensks_contracts::MessageState::Streaming,
            "running",
        ),
    };
    repo.set_message_content(
        &lease.assistant_message_id,
        &outcome.assistant_text,
        assistant_state,
        now_ms,
    )
    .map_err(|error| error.to_string())?;
    repo.finish_turn_supervisor_lease(
        &lease.turn_id,
        &lease.run_id,
        run_state,
        last_event_sequence,
        terminal_kind,
        now_ms,
    )
    .map_err(|error| error.to_string())?;

    Ok(serde_json::json!({
        "status": "executed",
        "run_state": run_state,
        "assistant_message_id": lease.assistant_message_id,
        "last_event_sequence": last_event_sequence,
        "patch_count": outcome.patches.len(),
        "apply_result_count": outcome.apply_results.len(),
    }))
}

struct DaemonAgentEventSink {
    store: std::sync::Mutex<opensks_event_store::EventStore>,
    failures: std::sync::Mutex<Vec<String>>,
}

impl DaemonAgentEventSink {
    fn open(workspace: &Path) -> Result<Self, DaemonError> {
        let store = opensks_event_store::EventStore::open_workspace(workspace)
            .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
        Ok(Self {
            store: std::sync::Mutex::new(store),
            failures: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn emit_run_started(
        &self,
        request: &opensks_adapter::AgentRunRequest,
    ) -> Result<(), DaemonError> {
        let event = opensks_contracts::ExecutionEventEnvelope {
            schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!("agent-{}-run-started", request.run_id),
            run_id: request.run_id.clone(),
            sequence: 1,
            occurred_at: event_time_from_ms(request.now_ms),
            actor: "turn-supervisor".to_string(),
            causation_id: Some(request.turn_id.clone()),
            correlation_id: Some(request.stream_id.clone()),
            kind: opensks_contracts::EventKind::RunStarted,
            payload: serde_json::json!({
                "source": "conversation.supervisor.tick",
                "project_id": request.project_id,
                "conversation_id": request.conversation_id,
                "turn_id": request.turn_id,
                "stream_id": request.stream_id,
                "state": "running"
            }),
            sensitivity: opensks_contracts::Sensitivity::Internal,
            evidence_refs: vec!["conversation:turn-supervisor".to_string()],
        };
        let mut store = self
            .store
            .lock()
            .map_err(|_| DaemonError::Io(std::io::Error::other("event journal lock poisoned")))?;
        store
            .append_event(event)
            .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
        Ok(())
    }

    fn finish(&self, run_id: &str) -> Result<u64, DaemonError> {
        let failures = self.failures.lock().map_err(|_| {
            DaemonError::Io(std::io::Error::other("event journal failure lock poisoned"))
        })?;
        if !failures.is_empty() {
            return Err(DaemonError::Io(std::io::Error::other(format!(
                "append agent event journal: {}",
                failures.join("; ")
            ))));
        }
        drop(failures);
        let store = self
            .store
            .lock()
            .map_err(|_| DaemonError::Io(std::io::Error::other("event journal lock poisoned")))?;
        let events = store
            .replay(run_id)
            .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
        Ok(events.last().map(|event| event.sequence).unwrap_or(0))
    }

    fn record_failure(&self, error: impl std::fmt::Display) {
        if let Ok(mut failures) = self.failures.lock() {
            failures.push(error.to_string());
        }
    }
}

impl opensks_adapter::AgentEventSink for DaemonAgentEventSink {
    fn emit(&self, event: opensks_contracts::AgentEventEnvelope) {
        let execution_event = execution_event_from_agent_event(event);
        match self.store.lock() {
            Ok(mut store) => {
                if let Err(error) = store.append_event(execution_event) {
                    self.record_failure(error);
                }
            }
            Err(_) => self.record_failure("event journal lock poisoned"),
        }
    }
}

fn execution_event_from_agent_event(
    event: opensks_contracts::AgentEventEnvelope,
) -> opensks_contracts::ExecutionEventEnvelope {
    let agent_kind = agent_event_kind_label(event.kind);
    opensks_contracts::ExecutionEventEnvelope {
        schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("agent-{}-s{}", event.run_id, event.sequence + 2),
        run_id: event.run_id.clone(),
        sequence: event.sequence + 2,
        occurred_at: event_time_from_ms(event.occurred_at_ms),
        actor: event
            .worker_id
            .clone()
            .unwrap_or_else(|| "agent".to_string()),
        causation_id: Some(format!("agent-event:{}", event.sequence)),
        correlation_id: Some(event.stream_id.clone()),
        kind: execution_kind_for_agent_event(event.kind),
        payload: serde_json::json!({
            "source_schema": event.schema,
            "agent_event_kind": agent_kind,
            "stream_id": event.stream_id,
            "project_id": event.project_id,
            "conversation_id": event.conversation_id,
            "turn_id": event.turn_id,
            "run_id": event.run_id,
            "worker_id": event.worker_id,
            "node_id": event.node_id,
            "payload": event.payload
        }),
        sensitivity: event.sensitivity,
        evidence_refs: event.evidence_refs,
    }
}

fn execution_kind_for_agent_event(
    kind: opensks_contracts::AgentEventKind,
) -> opensks_contracts::EventKind {
    use opensks_contracts::AgentEventKind;
    match kind {
        AgentEventKind::AssistantTextCompleted
        | AgentEventKind::ToolCallCompleted
        | AgentEventKind::FilePatchApplied
        | AgentEventKind::WorkerCompleted => opensks_contracts::EventKind::WorkItemCompleted,
        AgentEventKind::VerificationCompleted => opensks_contracts::EventKind::VerificationPassed,
        AgentEventKind::Error => opensks_contracts::EventKind::VerificationFailed,
        AgentEventKind::ApprovalRequested => opensks_contracts::EventKind::ApprovalRequested,
        AgentEventKind::ApprovalResolved => opensks_contracts::EventKind::ApprovalApproved,
        AgentEventKind::PlanUpdated
        | AgentEventKind::AssistantTextDelta
        | AgentEventKind::ToolCallStarted
        | AgentEventKind::ToolCallOutput
        | AgentEventKind::FilePatchProposed
        | AgentEventKind::VerificationStarted
        | AgentEventKind::WorkerSpawned
        | AgentEventKind::WorkerProgress
        | AgentEventKind::ImageArtifactCreated
        | AgentEventKind::Warning => opensks_contracts::EventKind::WorkItemRunning,
    }
}

fn setup_required_agent_event(
    request: &opensks_adapter::AgentRunRequest,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("provider-router".to_string()),
        node_id: None,
        sequence: 0,
        occurred_at_ms: request.now_ms,
        kind: opensks_contracts::AgentEventKind::Error,
        payload: serde_json::json!({
            "code": "setup_required",
            "message": "Needs setup — connect at least one code-capable model."
        }),
        sensitivity: opensks_contracts::Sensitivity::Public,
        evidence_refs: vec!["provider:no-code-capable-model".to_string()],
    }
}

#[cfg(any(test, feature = "simulation"))]
fn run_explicit_local_test_turn(
    request: &opensks_adapter::AgentRunRequest,
    sink: &dyn opensks_adapter::AgentEventSink,
) -> Result<opensks_adapter::AgentRunOutcome, opensks_adapter::AgentAdapterError> {
    opensks_adapter::AgentAdapter::run(&opensks_adapter::LocalTestAdapter::new(), request, sink)
}

#[cfg(not(any(test, feature = "simulation")))]
fn run_explicit_local_test_turn(
    request: &opensks_adapter::AgentRunRequest,
    sink: &dyn opensks_adapter::AgentEventSink,
) -> Result<opensks_adapter::AgentRunOutcome, opensks_adapter::AgentAdapterError> {
    opensks_adapter::AgentEventSink::emit(
        sink,
        opensks_contracts::AgentEventEnvelope {
            schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
            stream_id: request.stream_id.clone(),
            project_id: request.project_id.clone(),
            conversation_id: request.conversation_id.clone(),
            turn_id: request.turn_id.clone(),
            run_id: request.run_id.clone(),
            worker_id: Some("simulation-disabled".to_string()),
            node_id: None,
            sequence: 0,
            occurred_at_ms: request.now_ms,
            kind: opensks_contracts::AgentEventKind::Error,
            payload: serde_json::json!({
                "code": "simulation_unavailable",
                "message": "Local test simulation is disabled in this build."
            }),
            sensitivity: opensks_contracts::Sensitivity::Public,
            evidence_refs: vec!["build:simulation-feature-disabled".to_string()],
        },
    );
    Ok(opensks_adapter::AgentRunOutcome {
        assistant_text: "Local test simulation is disabled in this build.".to_string(),
        patches: vec![],
        apply_results: vec![],
        final_state: opensks_contracts::projection::RunProjectionState::Failed,
    })
}

fn agent_event_kind_label(kind: opensks_contracts::AgentEventKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

fn event_time_from_ms(ms: u64) -> String {
    let secs = ms / 1_000;
    let nanos = (ms % 1_000) * 1_000_000;
    format!("{secs}.{nanos:09}")
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
    use opensks_contracts::{
        CONVERSATION_THREAD_SETTINGS_SCHEMA, CONVERSATION_TURN_ACCEPTED_SCHEMA,
        CONVERSATION_TURN_START_REQUEST_SCHEMA, ConversationStatus, ConversationThreadSettings,
        ConversationTurnAccepted, ConversationTurnSettings, EngineRequest, EngineRequestKind,
        EngineRequestParams, ExecutionMode, MessageRole, MessageState, ModelSelection,
        ModelSelectionMode, ReasoningEffort, RunProjectionState, TurnContextSelection,
        UserMessageInput,
    };
    use opensks_conversation::ConversationRepository;
    use std::io::Cursor;

    fn temp_workspace(name: &str) -> PathBuf {
        let workspace =
            std::env::temp_dir().join(format!("opensks-daemon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        workspace
    }

    fn seed_conversation(workspace: &Path) -> (String, String) {
        let repo = ConversationRepository::open_workspace(workspace).expect("repo");
        let project_id = repo
            .create_project("workspace-test", "Workspace Test", 1_000)
            .expect("project");
        let conversation_id = repo
            .create_conversation(&project_id, "Daemon turn", 1_100)
            .expect("conversation");
        (project_id, conversation_id)
    }

    fn turn_start_request(
        project_id: &str,
        conversation_id: &str,
        request_id: &str,
        idempotency_key: &str,
    ) -> opensks_contracts::ConversationTurnStartRequest {
        opensks_contracts::ConversationTurnStartRequest {
            schema: CONVERSATION_TURN_START_REQUEST_SCHEMA.to_string(),
            request_id: request_id.to_string(),
            project_id: project_id.to_string(),
            conversation_id: conversation_id.to_string(),
            client_turn_id: format!("client-{request_id}"),
            message: UserMessageInput {
                text: "start a durable daemon turn".to_string(),
                attachment_refs: vec![],
            },
            settings: ConversationTurnSettings {
                model: ModelSelection {
                    mode: ModelSelectionMode::Auto,
                    model_id: None,
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Standard,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "auto".to_string(),
                graph_revision: None,
                max_parallelism: 4,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: None,
            },
            context: TurnContextSelection::default(),
            idempotency_key: idempotency_key.to_string(),
        }
    }

    fn accepted_lines(output: &str) -> Vec<ConversationTurnAccepted> {
        output
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                value.get("schema").and_then(serde_json::Value::as_str)
                    == Some(CONVERSATION_TURN_ACCEPTED_SCHEMA)
            })
            .map(|value| serde_json::from_value(value).expect("accepted event"))
            .collect()
    }

    fn supervisor_tick_lines(output: &str) -> Vec<serde_json::Value> {
        output
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                value.get("schema").and_then(serde_json::Value::as_str)
                    == Some("opensks.turn-supervisor-tick.v1")
            })
            .collect()
    }

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
    fn conversation_turn_start_returns_accepted_handle_without_adapter_execution() {
        let workspace = temp_workspace("conversation-turn-start");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-turn-start",
            "idem-conversation-turn-start",
        );
        let request = EngineRequest::conversation_turn_start(turn_request);
        let input = serde_json::to_string(&request).expect("request");

        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        let accepted = &accepted[0];
        assert_eq!(accepted.request_id, "req-conversation-turn-start");
        assert_eq!(accepted.state, RunProjectionState::Queued);
        assert!(accepted.run_id.starts_with("turn-"));
        assert_eq!(accepted.stream_id, format!("stream-{}", accepted.turn_id));
        assert!(output.contains("\"event_type\":\"request_completed\""));
        assert!(!output.contains("Needs setup"));
        assert!(!output.contains("LocalTest"));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));

        let last = output
            .lines()
            .rfind(|line| !line.trim().is_empty())
            .expect("last line");
        assert!(last.contains("\"event_type\":\"request_completed\""));
        assert!(last.contains("\"request_id\":\"req-conversation-turn-start\""));

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[0].state, MessageState::Complete);
        assert_eq!(messages[1].role, MessageRole::Assistant);
        assert_eq!(messages[1].state, MessageState::Streaming);

        let runs = repo
            .runs_for_conversation(&conversation_id)
            .expect("conversation runs");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, accepted.run_id);
        assert_eq!(runs[0].message_id, accepted.assistant_message_id);
        assert_eq!(runs[0].run_state.as_deref(), Some("queued"));
        assert_eq!(
            repo.run_last_event_sequence(&accepted.run_id)
                .expect("run sequence"),
            Some(0)
        );
        let routing_raw = repo
            .turn_model_routing_decision_json(&accepted.turn_id)
            .expect("routing decision")
            .expect("routing decision snapshot");
        let routing: opensks_contracts::RoutingDecision =
            serde_json::from_str(&routing_raw).expect("routing json");
        assert_eq!(
            routing.status,
            opensks_contracts::RoutingStatus::BlockedMissingCapability
        );
        assert_eq!(routing.reason_code, "thread_settings_model_not_selected");
        let receipt = routing.route_receipt.expect("route receipt");
        assert_eq!(receipt.reason_code, "thread_settings_model_not_selected");
        assert!(receipt.provider_id.is_none());
        assert_eq!(receipt.registry_revision, routing.model_snapshot_hash);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_executes_claimed_local_test_turn() {
        let workspace = temp_workspace("conversation-supervisor-tick");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-supervisor-start",
            "idem-conversation-supervisor-start",
        );
        turn_request.message.text = r#"{"local_test":{"op":"create_file","path":"SUPERVISOR_NOTE.md","value":"written by daemon supervisor"}}"#.to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-supervisor-tick",
            "daemon-test-supervisor",
            1_000,
        );
        let input = [
            serde_json::to_string(&start).expect("start request"),
            serde_json::to_string(&tick).expect("tick request"),
        ]
        .join("\n");

        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["supervisor_id"], "daemon-test-supervisor");
        assert_eq!(ticks[0]["claimed"]["run_id"], accepted[0].run_id);
        assert_eq!(ticks[0]["executed"]["status"], "executed");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");
        assert!(
            ticks[0]["executed"]["last_event_sequence"]
                .as_u64()
                .unwrap_or(0)
                >= 4
        );

        let written =
            std::fs::read_to_string(workspace.join("SUPERVISOR_NOTE.md")).expect("written file");
        assert_eq!(written, "written by daemon supervisor");
        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert_eq!(messages[1].state, MessageState::Complete);
        assert!(messages[1].content_redacted.contains("SUPERVISOR_NOTE.md"));
        assert_eq!(
            repo.run_projection_state(&accepted[0].run_id)
                .expect("run state")
                .as_deref(),
            Some("completed")
        );
        assert_eq!(
            repo.get_conversation(&conversation_id)
                .expect("conversation")
                .expect("conversation present")
                .status,
            ConversationStatus::Completed
        );
        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted[0].run_id).expect("replay events");
        assert_eq!(
            events.first().map(|event| &event.kind),
            Some(&EventKind::RunStarted)
        );
        assert!(
            events.iter().any(|event| {
                event.payload["agent_event_kind"] == "file_patch_applied"
                    && event.kind == EventKind::WorkItemCompleted
            }),
            "supervisor execution must persist patch events: {events:#?}"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_fails_setup_required_without_model_or_simulation() {
        let workspace = temp_workspace("conversation-supervisor-setup-required");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let start = EngineRequest::conversation_turn_start(turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-supervisor-setup-start",
            "idem-conversation-supervisor-setup-start",
        ));
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-supervisor-setup-tick",
            "daemon-test-supervisor",
            1_000,
        );
        let input = [
            serde_json::to_string(&start).expect("start request"),
            serde_json::to_string(&tick).expect("tick request"),
        ]
        .join("\n");

        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["executed"]["status"], "executed");
        assert_eq!(ticks[0]["executed"]["run_state"], "failed");

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert_eq!(messages[1].state, MessageState::Failed);
        assert!(messages[1].content_redacted.contains("Needs setup"));
        assert_eq!(
            repo.run_projection_state(&accepted[0].run_id)
                .expect("run state")
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            repo.get_conversation(&conversation_id)
                .expect("conversation")
                .expect("conversation present")
                .status,
            ConversationStatus::Failed
        );
        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted[0].run_id).expect("replay events");
        assert!(
            events.iter().any(|event| {
                event.kind == EventKind::VerificationFailed
                    && event.payload["agent_event_kind"] == "error"
                    && event.payload["payload"]["code"] == "setup_required"
            }),
            "setup-required failure must be durable: {events:#?}"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_turn_start_snapshots_persisted_thread_settings() {
        let workspace = temp_workspace("conversation-turn-start-thread-settings");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Pinned,
                    model_id: Some("openrouter/daemon-code-model".to_string()),
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Maximum,
                execution_mode: ExecutionMode::ReadOnly,
                pipeline_id: "daemon-thread-pipeline".to_string(),
                max_parallelism: 9,
                verifier_count: 4,
                tool_policy_id: "daemon-tools".to_string(),
                approval_policy_id: "daemon-approval".to_string(),
                image_model_id: None,
                updated_at_ms: 1_900,
            };
            repo.set_thread_settings(
                &conversation_id,
                &serde_json::to_string(&thread_settings).expect("settings json"),
                1_900,
            )
            .expect("set settings");
        }

        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-turn-start-settings",
            "idem-conversation-turn-start-settings",
        );
        turn_request.settings.pipeline_id = "client-request-ignored".to_string();
        turn_request.settings.max_parallelism = 99;
        let request = EngineRequest::conversation_turn_start(turn_request);
        let input = serde_json::to_string(&request).expect("request");

        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let settings_raw = repo
            .turn_effective_settings_json(&accepted[0].turn_id)
            .expect("turn settings")
            .expect("turn settings snapshot");
        let settings: ConversationTurnSettings =
            serde_json::from_str(&settings_raw).expect("effective settings");
        assert_eq!(settings.pipeline_id, "daemon-thread-pipeline");
        assert_eq!(settings.max_parallelism, 9);
        assert_eq!(settings.verifier_count, 4);
        assert_eq!(settings.reasoning_effort, ReasoningEffort::Maximum);
        assert_eq!(settings.execution_mode, ExecutionMode::ReadOnly);
        assert_eq!(
            repo.run_projection_pipeline_id(&accepted[0].run_id)
                .expect("projection pipeline")
                .as_deref(),
            Some("daemon-thread-pipeline")
        );
        let routing_raw = repo
            .turn_model_routing_decision_json(&accepted[0].turn_id)
            .expect("routing decision")
            .expect("routing decision snapshot");
        let routing: opensks_contracts::RoutingDecision =
            serde_json::from_str(&routing_raw).expect("routing json");
        assert_eq!(routing.status, opensks_contracts::RoutingStatus::Routed);
        assert_eq!(
            routing.selected_model_id.as_deref(),
            Some("openrouter/daemon-code-model")
        );
        assert_eq!(routing.reason_code, "explicit_thread_settings_model");
        let receipt = routing.route_receipt.expect("route receipt");
        assert_eq!(receipt.provider_id.as_deref(), Some("openrouter"));
        assert_eq!(
            receipt.model_id.as_deref(),
            Some("openrouter/daemon-code-model")
        );
        assert_eq!(receipt.registry_revision, routing.model_snapshot_hash);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_turn_start_idempotency_reuses_accepted_handle() {
        let workspace = temp_workspace("conversation-turn-start-idempotency");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let first = EngineRequest::conversation_turn_start(turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-turn-start-1",
            "idem-conversation-turn-start-replay",
        ));
        let replay = EngineRequest::conversation_turn_start(turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-turn-start-2",
            "idem-conversation-turn-start-replay",
        ));
        let input = [
            serde_json::to_string(&first).expect("first request"),
            serde_json::to_string(&replay).expect("replay request"),
        ]
        .join("\n");

        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 2);
        assert_eq!(accepted[0].request_id, "req-conversation-turn-start-1");
        assert_eq!(accepted[1].request_id, "req-conversation-turn-start-2");
        assert_eq!(accepted[0].turn_id, accepted[1].turn_id);
        assert_eq!(accepted[0].run_id, accepted[1].run_id);
        assert_eq!(accepted[0].user_message_id, accepted[1].user_message_id);
        assert_eq!(
            accepted[0].assistant_message_id,
            accepted[1].assistant_message_id
        );
        assert_eq!(
            output
                .matches("\"event_type\":\"request_completed\"")
                .count(),
            2
        );

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        assert_eq!(
            repo.message_page(&conversation_id, None, 10)
                .expect("messages")
                .len(),
            2
        );
        assert_eq!(
            repo.runs_for_conversation(&conversation_id)
                .expect("runs")
                .len(),
            1
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_turn_start_missing_param_returns_error_and_terminal_marker() {
        let request = EngineRequest {
            schema: opensks_contracts::ENGINE_REQUEST_SCHEMA.to_string(),
            id: "req-conversation-turn-start-missing".to_string(),
            kind: EngineRequestKind::ConversationTurnStart,
            protocol_version: opensks_contracts::CONTRACT_VERSION.to_string(),
            params: EngineRequestParams::default(),
        };
        let input = serde_json::to_string(&request).expect("request");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: PathBuf::from("."),
            },
        )
        .expect("stdio");

        assert!(accepted_lines(&output).is_empty());
        assert!(output.contains("\"event_type\":\"error\""));
        assert!(output.contains("params.conversation_turn_start"));
        let last = output
            .lines()
            .rfind(|line| !line.trim().is_empty())
            .expect("last line");
        assert!(last.contains("\"event_type\":\"request_completed\""));
        assert!(last.contains("\"request_id\":\"req-conversation-turn-start-missing\""));
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
