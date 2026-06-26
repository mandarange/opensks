use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opensks_contracts::{
    EngineEvent, EngineEventType, EngineRequest, EngineRequestKind, EventKind, EventSeverity,
    ExecutionEventEnvelope, PipelineGraph, PublicEngineError,
};
use opensks_stream::{StreamFramer, encode_frame};
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
const MAX_INTEGRATION_VERIFIER_LANES: usize = 8;
const RESIDENT_TURN_SUPERVISOR_ID: &str = "daemon-resident-turn-supervisor";
const RESIDENT_SUPERVISOR_LEASE_TTL_MS: u64 = 30_000;
const RESIDENT_SUPERVISOR_WAKEUP_DEBOUNCE_MS: u64 = 25;
const RESIDENT_SUPERVISOR_EXPLICIT_TICK_GRACE_MS: u64 = 50;
const RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS: usize = 8;
const RESIDENT_SUPERVISOR_STARTUP_QUIESCE_MAX_MS: u64 = 500;
const RESIDENT_SUPERVISOR_STARTUP_QUIESCE_POLL_MS: u64 = 5;
const WORKTREE_ISOLATION_INVENTORY_KIND: &str = "worktree_inventory";
const WORKTREE_ISOLATION_RECOVERY_KIND: &str = "worktree_recover";
const WORKTREE_ISOLATION_INVENTORY_LEGACY_KIND: &str = "worktree_isolation_inventory";
const RAW_PROMPT_VAULT_KEYCHAIN_SERVICE: &str = "dev.opensks.raw-prompt-vault";
const WORKTREE_ISOLATION_RECOVERY_LEGACY_KIND: &str = "worktree_isolation_recovery";

static DAEMON_ARTIFACT_WRITE_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("could not encode daemon event: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("stdio error: {0}")]
    Io(#[from] std::io::Error),
    #[error("conversation repository error: {0}")]
    Conversation(#[from] opensks_conversation::ConversationError),
    #[error("stream framing error: {0}")]
    StreamFrame(#[from] opensks_stream::StreamError),
    #[error("stream response channel closed")]
    StreamClosed,
    #[error("stream worker panicked")]
    StreamWorkerPanic,
}

enum StreamResponse {
    Lines(Vec<String>),
}

#[derive(Debug, Default)]
struct ResidentSupervisorDrainState {
    active: AtomicBool,
    requested: AtomicBool,
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
        let resident_drain_state = Arc::new(ResidentSupervisorDrainState::default());
        if workspace_has_conversation_store(options) {
            let tx = tx.clone();
            let startup_options = options.clone();
            let startup_active_requests = active_requests.clone();
            let startup_drain_state = resident_drain_state.clone();
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(
                    RESIDENT_SUPERVISOR_WAKEUP_DEBOUNCE_MS,
                ));
                if !wait_for_stream_request_quiescence(&startup_active_requests) {
                    return;
                }
                let resident_wakeup = EngineRequest::conversation_supervisor_tick(
                    "resident-supervisor-startup",
                    RESIDENT_TURN_SUPERVISOR_ID,
                    RESIDENT_SUPERVISOR_LEASE_TTL_MS,
                );
                let resident_lines = resident_supervisor_coalesced_drain_lines(
                    0,
                    resident_wakeup,
                    &startup_options,
                    &startup_drain_state,
                );
                if !resident_lines.is_empty() {
                    let _ = tx.send(StreamResponse::Lines(resident_lines));
                }
            });
        }

        let explicit_supervisor_tick_generation = Arc::new(AtomicUsize::new(0));
        let explicit_supervisor_claim_generation = Arc::new(AtomicUsize::new(0));
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
            let explicit_tick_generation_at_dispatch =
                explicit_supervisor_tick_generation.load(Ordering::SeqCst);
            let explicit_claim_generation_at_dispatch =
                explicit_supervisor_claim_generation.load(Ordering::SeqCst);
            let is_explicit_supervisor_tick = request_is_conversation_supervisor_tick(&request);
            if is_explicit_supervisor_tick {
                explicit_supervisor_tick_generation.fetch_add(1, Ordering::SeqCst);
            }
            let options = options.clone();
            let tx = tx.clone();
            let active_requests = active_requests.clone();
            let resident_drain_state = resident_drain_state.clone();
            let explicit_supervisor_tick_generation = explicit_supervisor_tick_generation.clone();
            let explicit_supervisor_claim_generation = explicit_supervisor_claim_generation.clone();
            scope.spawn(move || {
                let lines = request_lines(request_idx, &request, &options).unwrap_or_else(|error| {
                    vec![
                        serde_json::to_string(&stream_request_error_event(request_idx, error))
                            .unwrap_or_else(|_| {
                                "{\"schema\":\"opensks.engine-event.v1\",\"event_type\":\"error\",\"message\":\"stream request failed\"}".to_string()
                            }),
                    ]
                });
                if is_explicit_supervisor_tick && response_lines_contain_supervisor_claim(&lines) {
                    explicit_supervisor_claim_generation.fetch_add(1, Ordering::SeqCst);
                }
                let resident_wakeup = resident_supervisor_wakeup_request(&request, &lines);
                let _ = tx.send(StreamResponse::Lines(lines));
                active_requests.fetch_sub(1, Ordering::SeqCst);
                let Some(resident_wakeup) = resident_wakeup else {
                    return;
                };
                std::thread::sleep(Duration::from_millis(
                    RESIDENT_SUPERVISOR_WAKEUP_DEBOUNCE_MS,
                ));
                if explicit_supervisor_tick_generation.load(Ordering::SeqCst)
                    != explicit_tick_generation_at_dispatch
                {
                    std::thread::sleep(Duration::from_millis(
                        RESIDENT_SUPERVISOR_EXPLICIT_TICK_GRACE_MS,
                    ));
                    if explicit_supervisor_claim_generation.load(Ordering::SeqCst)
                        != explicit_claim_generation_at_dispatch
                    {
                        return;
                    }
                    if !wait_for_stream_request_quiescence(&active_requests) {
                        return;
                    }
                }
                if explicit_supervisor_claim_generation.load(Ordering::SeqCst)
                    != explicit_claim_generation_at_dispatch
                {
                    return;
                }
                let resident_lines = resident_supervisor_coalesced_drain_lines(
                    request_idx,
                    resident_wakeup,
                    &options,
                    &resident_drain_state,
                );
                if !resident_lines.is_empty() {
                    let _ = tx.send(StreamResponse::Lines(resident_lines));
                }
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

fn request_is_conversation_supervisor_tick(trimmed: &str) -> bool {
    serde_json::from_str::<EngineRequest>(trimmed)
        .map(|request| matches!(request.kind, EngineRequestKind::ConversationSupervisorTick))
        .unwrap_or(false)
}

fn workspace_has_conversation_store(options: &DaemonOptions) -> bool {
    options
        .workspace
        .join(opensks_conversation::CONVERSATION_DB_RELATIVE_PATH)
        .exists()
}

fn wait_for_stream_request_quiescence(active_requests: &AtomicUsize) -> bool {
    let deadline =
        Instant::now() + Duration::from_millis(RESIDENT_SUPERVISOR_STARTUP_QUIESCE_MAX_MS);
    while active_requests.load(Ordering::SeqCst) > 0 {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(
            RESIDENT_SUPERVISOR_STARTUP_QUIESCE_POLL_MS,
        ));
    }
    true
}

fn resident_supervisor_wakeup_request(
    trimmed: &str,
    response_lines: &[String],
) -> Option<EngineRequest> {
    let request = serde_json::from_str::<EngineRequest>(trimmed).ok()?;
    if !matches!(request.kind, EngineRequestKind::ConversationTurnStart) {
        return None;
    }
    if !response_lines_contain_turn_accepted(response_lines) {
        return None;
    }
    Some(EngineRequest::conversation_supervisor_tick(
        format!("resident-supervisor-wakeup-{}", request.id),
        RESIDENT_TURN_SUPERVISOR_ID,
        RESIDENT_SUPERVISOR_LEASE_TTL_MS,
    ))
}

fn response_lines_contain_turn_accepted(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| {
                value
                    .get("schema")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .as_deref()
            == Some(opensks_contracts::CONVERSATION_TURN_ACCEPTED_SCHEMA)
    })
}

fn response_lines_contain_supervisor_claim(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .filter(|value| {
                value.get("schema").and_then(serde_json::Value::as_str)
                    == Some("opensks.turn-supervisor-tick.v1")
            })
            .and_then(|value| value.get("claimed").cloned())
            .is_some_and(|claimed| !claimed.is_null())
    })
}

fn response_lines_contain_supervisor_tick(lines: &[String]) -> bool {
    lines.iter().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| {
                value
                    .get("schema")
                    .and_then(serde_json::Value::as_str)
                    .map(|schema| schema == "opensks.turn-supervisor-tick.v1")
            })
            .unwrap_or(false)
    })
}

fn resident_supervisor_coalesced_drain_lines(
    request_idx: usize,
    first_request: EngineRequest,
    options: &DaemonOptions,
    drain_state: &ResidentSupervisorDrainState,
) -> Vec<String> {
    drain_state.requested.store(true, Ordering::SeqCst);
    if drain_state
        .active
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Vec::new();
    }

    let mut request = first_request;
    let mut output = Vec::new();
    let mut pass_index = 1usize;
    let burst_id = timestamp_ms();
    loop {
        drain_state.requested.store(false, Ordering::SeqCst);
        output.extend(resident_supervisor_drain_lines(
            request_idx,
            request,
            options,
        ));
        if drain_state.requested.load(Ordering::SeqCst) {
            pass_index += 1;
            request = resident_supervisor_coalesced_followup_request(burst_id, pass_index);
            continue;
        }

        drain_state.active.store(false, Ordering::SeqCst);
        if !drain_state.requested.swap(false, Ordering::SeqCst) {
            break;
        }
        if drain_state
            .active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            break;
        }
        pass_index += 1;
        request = resident_supervisor_coalesced_followup_request(burst_id, pass_index);
    }
    output
}

fn resident_supervisor_coalesced_followup_request(
    burst_id: u64,
    pass_index: usize,
) -> EngineRequest {
    EngineRequest::conversation_supervisor_tick(
        format!("resident-supervisor-coalesced-drain-{burst_id}-{pass_index}"),
        RESIDENT_TURN_SUPERVISOR_ID,
        RESIDENT_SUPERVISOR_LEASE_TTL_MS,
    )
}

fn resident_supervisor_drain_lines(
    request_idx: usize,
    first_request: EngineRequest,
    options: &DaemonOptions,
) -> Vec<String> {
    let base_request_id = first_request.id.clone();
    let mut request = first_request;
    let mut drained = Vec::new();
    for drain_idx in 0..RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS {
        let lines = resident_supervisor_wakeup_lines(request_idx, request, options);
        if !response_lines_contain_supervisor_claim(&lines) {
            if !response_lines_contain_supervisor_tick(&lines) {
                drained.extend(lines);
            }
            break;
        }
        drained.extend(lines);
        request = EngineRequest::conversation_supervisor_tick(
            format!("{base_request_id}-drain-{}", drain_idx + 2),
            RESIDENT_TURN_SUPERVISOR_ID,
            RESIDENT_SUPERVISOR_LEASE_TTL_MS,
        );
    }
    drained
}

fn resident_supervisor_wakeup_lines(
    request_idx: usize,
    request: EngineRequest,
    options: &DaemonOptions,
) -> Vec<String> {
    let request_id = request.id.clone();
    serde_json::to_string(&request)
        .map_err(DaemonError::from)
        .and_then(|serialized| request_lines(request_idx, &serialized, options))
        .unwrap_or_else(|error| {
            let mut lines = vec![serde_json::to_string(&run_start_error_event(
                Some(request_id.clone()),
                error,
            ))
            .unwrap_or_else(|_| {
                "{\"schema\":\"opensks.engine-event.v1\",\"event_type\":\"error\",\"message\":\"resident supervisor wakeup failed\"}".to_string()
            })];
            if let Ok(marker) = serde_json::to_string(&request_completed_event(request_id)) {
                lines.push(marker);
            }
            lines
        })
}

fn request_lines(
    idx: usize,
    trimmed: &str,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let raw_request = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(request) => request,
        // An unparseable request has no id to correlate a terminal marker to; the
        // client's hard deadline is the only safety net for this defensive path.
        Err(error) => return Ok(vec![serde_json::to_string(&parse_error_event(idx, error))?]),
    };
    if let Some((request_id, mut lines)) =
        worktree_isolation_request_lines(idx, &raw_request, options)?
    {
        lines.push(serde_json::to_string(&request_completed_event(request_id))?);
        return Ok(lines);
    }
    let request = match serde_json::from_value::<EngineRequest>(raw_request) {
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
        EngineRequestKind::IntegrationCandidateApply => {
            match integration_candidate_apply_lines(&request, options) {
                Ok(apply_lines) => apply_lines,
                Err(error) => vec![serde_json::to_string(&run_start_error_event(
                    Some(request_id.clone()),
                    error,
                ))?],
            }
        }
        EngineRequestKind::WorktreeInventory => {
            match worktree_inventory_engine_request_lines(&request, options) {
                Ok(worktree_lines) => worktree_lines,
                Err(error) => vec![serde_json::to_string(&run_start_error_event(
                    Some(request_id.clone()),
                    error,
                ))?],
            }
        }
        EngineRequestKind::WorktreeRecover => {
            match worktree_recover_engine_request_lines(&request, options) {
                Ok(worktree_lines) => worktree_lines,
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

fn worktree_isolation_request_lines(
    idx: usize,
    raw_request: &serde_json::Value,
    options: &DaemonOptions,
) -> Result<Option<(String, Vec<String>)>, DaemonError> {
    let Some(kind) = raw_request.get("kind").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let operation = match kind {
        WORKTREE_ISOLATION_INVENTORY_KIND | WORKTREE_ISOLATION_INVENTORY_LEGACY_KIND => "inventory",
        WORKTREE_ISOLATION_RECOVERY_KIND | WORKTREE_ISOLATION_RECOVERY_LEGACY_KIND => "recovery",
        _ => return Ok(None),
    };
    let request_id = raw_request
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("worktree-isolation-request-{idx}"));
    let params = raw_request
        .get("params")
        .and_then(serde_json::Value::as_object);
    let workspace_param = params
        .and_then(|params| params.get("workspace"))
        .and_then(serde_json::Value::as_str);
    let Some(run_id_param) = params
        .and_then(|params| params.get("run_id"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(Some((
            request_id.clone(),
            vec![serde_json::to_string(&missing_param_event(
                &request_id,
                "run_id",
            ))?],
        )));
    };
    let workspace = match workspace_param {
        Some(workspace_param) => validate_worktree_isolation_workspace(options, workspace_param),
        None => daemon_workspace_for_worktree_isolation(options),
    };
    let workspace = match workspace {
        Ok(workspace) => workspace,
        Err(reason) => {
            return Ok(Some((
                request_id.clone(),
                vec![serde_json::to_string(&worktree_isolation_error_event(
                    &request_id,
                    operation,
                    "workspace_rejected",
                    reason,
                ))?],
            )));
        }
    };
    let run_id = match validate_worktree_isolation_run_id(run_id_param) {
        Ok(run_id) => run_id,
        Err(reason) => {
            return Ok(Some((
                request_id.clone(),
                vec![serde_json::to_string(&worktree_isolation_error_event(
                    &request_id,
                    operation,
                    "run_id_rejected",
                    reason,
                ))?],
            )));
        }
    };

    let lines = match operation {
        "inventory" => worktree_isolation_inventory_lines(&request_id, &workspace, run_id)?,
        "recovery" => worktree_isolation_recovery_lines(&request_id, &workspace, run_id)?,
        _ => Vec::new(),
    };
    Ok(Some((request_id, lines)))
}

fn worktree_inventory_engine_request_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let Some(run_id_param) = request.params.run_id.as_deref() else {
        return Ok(vec![serde_json::to_string(&missing_param_event(
            &request.id,
            "run_id",
        ))?]);
    };
    let workspace = match daemon_workspace_for_worktree_isolation(options) {
        Ok(workspace) => workspace,
        Err(reason) => {
            return Ok(vec![serde_json::to_string(
                &worktree_isolation_error_event(
                    &request.id,
                    "inventory",
                    "workspace_rejected",
                    reason,
                ),
            )?]);
        }
    };
    let run_id = match validate_worktree_isolation_run_id(run_id_param) {
        Ok(run_id) => run_id,
        Err(reason) => {
            return Ok(vec![serde_json::to_string(
                &worktree_isolation_error_event(
                    &request.id,
                    "inventory",
                    "run_id_rejected",
                    reason,
                ),
            )?]);
        }
    };
    worktree_isolation_inventory_lines(&request.id, &workspace, run_id)
}

fn worktree_recover_engine_request_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let Some(run_id_param) = request.params.run_id.as_deref() else {
        return Ok(vec![serde_json::to_string(&missing_param_event(
            &request.id,
            "run_id",
        ))?]);
    };
    let workspace = match daemon_workspace_for_worktree_isolation(options) {
        Ok(workspace) => workspace,
        Err(reason) => {
            return Ok(vec![serde_json::to_string(
                &worktree_isolation_error_event(
                    &request.id,
                    "recovery",
                    "workspace_rejected",
                    reason,
                ),
            )?]);
        }
    };
    let run_id = match validate_worktree_isolation_run_id(run_id_param) {
        Ok(run_id) => run_id,
        Err(reason) => {
            return Ok(vec![serde_json::to_string(
                &worktree_isolation_error_event(&request.id, "recovery", "run_id_rejected", reason),
            )?]);
        }
    };
    worktree_isolation_recovery_lines(&request.id, &workspace, run_id)
}

fn daemon_workspace_for_worktree_isolation(
    options: &DaemonOptions,
) -> Result<PathBuf, &'static str> {
    options
        .workspace
        .canonicalize()
        .map_err(|_| "daemon workspace is not readable")
}

fn validate_worktree_isolation_workspace(
    options: &DaemonOptions,
    workspace_param: &str,
) -> Result<PathBuf, &'static str> {
    let trimmed = workspace_param.trim();
    if trimmed.is_empty() || trimmed != workspace_param {
        return Err("workspace must be an absolute daemon workspace path");
    }
    let requested = Path::new(trimmed);
    if !requested.is_absolute()
        || requested
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err("workspace must be an absolute daemon workspace path");
    }
    let requested = requested
        .canonicalize()
        .map_err(|_| "workspace is not readable")?;
    let daemon_workspace = daemon_workspace_for_worktree_isolation(options)?;
    if requested != daemon_workspace {
        return Err("workspace must match the daemon workspace");
    }
    Ok(daemon_workspace)
}

fn validate_worktree_isolation_run_id(run_id: &str) -> Result<&str, &'static str> {
    let trimmed = run_id.trim();
    if trimmed.is_empty() || trimmed != run_id || trimmed.len() > 128 {
        return Err("run_id must be a safe artifact segment");
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("run_id must be a safe artifact segment");
    }
    Ok(trimmed)
}

fn worktree_isolation_inventory_lines(
    request_id: &str,
    workspace: &Path,
    run_id: &str,
) -> Result<Vec<String>, DaemonError> {
    let generated_at_ms = timestamp_ms();
    let receipt = match opensks_git::inventory_isolations(workspace, run_id, generated_at_ms) {
        Ok(receipt) => receipt,
        Err(_) => {
            return Ok(vec![serde_json::to_string(
                &worktree_isolation_error_event(
                    request_id,
                    "inventory",
                    "inventory_failed",
                    "worktree isolation inventory failed",
                ),
            )?]);
        }
    };
    let receipt_path = worktree_isolation_artifact_path(workspace, run_id, "inventory.json");
    let body = serde_json::to_vec_pretty(&receipt)?;
    if write_daemon_json_artifact_atomic(&receipt_path, body).is_err() {
        return Ok(vec![serde_json::to_string(
            &worktree_isolation_error_event(
                request_id,
                "inventory",
                "receipt_write_failed",
                "worktree isolation receipt write failed",
            ),
        )?]);
    }
    if append_worktree_isolation_inventory_event(workspace, &receipt).is_err() {
        return Ok(vec![serde_json::to_string(
            &worktree_isolation_error_event(
                request_id,
                "inventory",
                "event_append_failed",
                "worktree isolation event append failed",
            ),
        )?]);
    }
    Ok(vec![
        serde_json::to_string(&worktree_isolation_completed_event(
            request_id,
            "inventory",
            &receipt.run_id,
            &receipt.state,
            &receipt.reason_code,
            &receipt.inventory_ref,
        ))?,
        serde_json::to_string(&receipt)?,
    ])
}

fn worktree_isolation_recovery_lines(
    request_id: &str,
    workspace: &Path,
    run_id: &str,
) -> Result<Vec<String>, DaemonError> {
    let generated_at_ms = timestamp_ms();
    let inventory = match opensks_git::inventory_isolations(workspace, run_id, generated_at_ms) {
        Ok(receipt) => receipt,
        Err(_) => {
            return Ok(vec![serde_json::to_string(
                &worktree_isolation_error_event(
                    request_id,
                    "recovery",
                    "inventory_failed",
                    "worktree isolation inventory failed",
                ),
            )?]);
        }
    };
    let inventory_path = worktree_isolation_artifact_path(workspace, run_id, "inventory.json");
    let inventory_body = serde_json::to_vec_pretty(&inventory)?;
    if write_daemon_json_artifact_atomic(&inventory_path, inventory_body).is_err() {
        return Ok(vec![serde_json::to_string(
            &worktree_isolation_error_event(
                request_id,
                "recovery",
                "inventory_receipt_write_failed",
                "worktree isolation inventory receipt write failed",
            ),
        )?]);
    }

    let recovery = match opensks_git::recover_isolations(workspace, run_id, generated_at_ms) {
        Ok(receipt) => receipt,
        Err(_) => {
            return Ok(vec![serde_json::to_string(
                &worktree_isolation_error_event(
                    request_id,
                    "recovery",
                    "recovery_failed",
                    "worktree isolation recovery failed",
                ),
            )?]);
        }
    };
    let recovery_path = worktree_isolation_artifact_path(workspace, run_id, "recovery.json");
    let recovery_body = serde_json::to_vec_pretty(&recovery)?;
    if write_daemon_json_artifact_atomic(&recovery_path, recovery_body).is_err() {
        return Ok(vec![serde_json::to_string(
            &worktree_isolation_error_event(
                request_id,
                "recovery",
                "recovery_receipt_write_failed",
                "worktree isolation recovery receipt write failed",
            ),
        )?]);
    }
    if append_worktree_isolation_recovery_event(workspace, &recovery).is_err() {
        return Ok(vec![serde_json::to_string(
            &worktree_isolation_error_event(
                request_id,
                "recovery",
                "event_append_failed",
                "worktree isolation event append failed",
            ),
        )?]);
    }
    Ok(vec![
        serde_json::to_string(&worktree_isolation_completed_event(
            request_id,
            "recovery",
            &recovery.run_id,
            &recovery.state,
            &recovery.reason_code,
            &recovery.recovery_ref,
        ))?,
        serde_json::to_string(&inventory)?,
        serde_json::to_string(&recovery)?,
    ])
}

fn worktree_isolation_artifact_path(workspace: &Path, run_id: &str, name: &str) -> PathBuf {
    workspace
        .join(".opensks")
        .join("runtime")
        .join("worktrees")
        .join(run_id)
        .join(name)
}

fn write_daemon_json_artifact_atomic(path: &Path, body: Vec<u8>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("receipt.json");
    let counter = DAEMON_ARTIFACT_WRITE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = path.with_file_name(format!(
        "{file_name}.{}.{}.tmp",
        std::process::id(),
        counter
    ));
    std::fs::write(&tmp, [&body[..], b"\n"].concat())?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn worktree_isolation_completed_event(
    request_id: &str,
    operation: &str,
    run_id: &str,
    state: &str,
    reason_code: &str,
    receipt_ref: &str,
) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-worktree-isolation-{operation}-{run_id}"),
        Some(request_id.to_string()),
        EngineEventType::ExecutionEvent,
        format!("worktree isolation {operation} completed: {state}/{reason_code}"),
        timestamp_ms(),
    );
    event.evidence_refs = vec![
        format!("daemon:worktree-isolation-{operation}"),
        receipt_ref.to_string(),
    ];
    event.redacted = true;
    event
}

fn worktree_isolation_error_event(
    request_id: &str,
    operation: &str,
    reason_code: &str,
    message: &str,
) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-worktree-isolation-{operation}-error"),
        Some(request_id.to_string()),
        EngineEventType::Error,
        message,
        timestamp_ms(),
    );
    event.severity = EventSeverity::Error;
    event.evidence_refs = vec![
        "daemon:worktree-isolation-request".to_string(),
        format!("daemon:worktree-isolation-{operation}"),
        format!("reason:{reason_code}"),
    ];
    event.redacted = true;
    event
}

fn append_worktree_isolation_inventory_event(
    workspace: &Path,
    receipt: &opensks_contracts::WorktreeIsolationInventoryReceipt,
) -> Result<(), String> {
    let event = opensks_contracts::ExecutionEventEnvelope {
        schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("worktree-isolation-inventory-{}", receipt.run_id),
        run_id: receipt.run_id.clone(),
        sequence: 0,
        occurred_at: event_time_from_ms(receipt.generated_at_ms),
        actor: "daemon-worktree-isolation".to_string(),
        causation_id: None,
        correlation_id: Some(receipt.id.clone()),
        kind: opensks_contracts::EventKind::SnapshotWritten,
        payload: serde_json::json!({
            "source": "daemon.worktree_isolation_inventory",
            "receipt_schema": receipt.schema,
            "state": receipt.state,
            "reason_code": receipt.reason_code,
            "inventory_ref": receipt.inventory_ref,
            "isolation_count": receipt.isolation_count,
            "git_available": receipt.git_available,
            "path_redacted": true,
            "content_redacted": true
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: receipt
            .evidence_refs
            .iter()
            .cloned()
            .chain(std::iter::once(
                "daemon:worktree-isolation-inventory".to_string(),
            ))
            .collect(),
    };
    let mut store = opensks_event_store::EventStore::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    store
        .append_event(event)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn append_worktree_isolation_recovery_event(
    workspace: &Path,
    receipt: &opensks_contracts::WorktreeIsolationRecoveryReceipt,
) -> Result<(), String> {
    let event = opensks_contracts::ExecutionEventEnvelope {
        schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("worktree-isolation-recovery-{}", receipt.run_id),
        run_id: receipt.run_id.clone(),
        sequence: 0,
        occurred_at: event_time_from_ms(receipt.generated_at_ms),
        actor: "daemon-worktree-isolation".to_string(),
        causation_id: None,
        correlation_id: Some(receipt.id.clone()),
        kind: opensks_contracts::EventKind::SnapshotWritten,
        payload: serde_json::json!({
            "source": "daemon.worktree_isolation_recovery",
            "receipt_schema": receipt.schema,
            "state": receipt.state,
            "reason_code": receipt.reason_code,
            "inventory_ref": receipt.inventory_ref,
            "recovery_ref": receipt.recovery_ref,
            "target_count": receipt.target_count,
            "recovered_count": receipt.recovered_count,
            "prune_attempted": receipt.prune_attempted,
            "prune_succeeded": receipt.prune_succeeded,
            "path_redacted": true,
            "content_redacted": true
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: receipt
            .evidence_refs
            .iter()
            .cloned()
            .chain(std::iter::once(
                "daemon:worktree-isolation-recovery".to_string(),
            ))
            .collect(),
    };
    let mut store = opensks_event_store::EventStore::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    store
        .append_event(event)
        .map(|_| ())
        .map_err(|error| error.to_string())
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

fn subscription_cursor_gap_event(
    request_id: &str,
    run_id: &str,
    requested_sequence: u64,
    last_sequence: u64,
) -> EngineEvent {
    let mut event = EngineEvent::new(
        format!("engine-subscribe-cursor-gap-{run_id}"),
        Some(request_id.to_string()),
        EngineEventType::Error,
        format!(
            "event stream cursor gap: requested sequence {requested_sequence} is beyond durable sequence {last_sequence}"
        ),
        timestamp_ms(),
    );
    event.severity = EventSeverity::Error;
    event.redacted = true;
    event.evidence_refs = vec![
        "daemon:subscription-cursor-gap".to_string(),
        "event-store:last-sequence".to_string(),
    ];
    event
}

#[derive(Debug, Clone)]
struct SubscriptionStreamMetadata {
    stream_id: String,
    project_id: String,
    conversation_id: String,
}

fn subscription_stream_metadata(workspace: &Path, run_id: &str) -> SubscriptionStreamMetadata {
    let fallback = SubscriptionStreamMetadata {
        stream_id: format!("event-stream-{run_id}"),
        project_id: "engine".to_string(),
        conversation_id: "engine".to_string(),
    };
    if !workspace
        .join(opensks_conversation::CONVERSATION_DB_RELATIVE_PATH)
        .exists()
    {
        return fallback;
    }
    let Ok(repo) = opensks_conversation::ConversationRepository::open_workspace(workspace) else {
        return fallback;
    };
    let Ok(Some(metadata)) = repo.stream_metadata_for_run(run_id) else {
        return fallback;
    };
    SubscriptionStreamMetadata {
        stream_id: metadata.stream_id,
        project_id: metadata.project_id,
        conversation_id: metadata.conversation_id,
    }
}

fn append_subscription_execution_event_lines(
    lines: &mut Vec<String>,
    framer: &mut StreamFramer,
    event: &ExecutionEventEnvelope,
) -> Result<(), DaemonError> {
    lines.push(serde_json::to_string(event)?);
    let frame = framer.event(serde_json::to_value(event)?)?;
    lines.push(encode_frame(&frame)?);
    Ok(())
}

fn subscription_stream_failed_frame(framer: &mut StreamFramer) -> Result<String, DaemonError> {
    let mut error =
        PublicEngineError::new("subscription_replay_failed", "event replay failed", true);
    error.evidence_refs = vec!["daemon:subscription-error".to_string()];
    let frame = framer.fail(error, true)?;
    Ok(encode_frame(&frame)?)
}

fn subscription_stream_gap_frame(
    framer: &mut StreamFramer,
    requested_sequence: u64,
    last_sequence: u64,
) -> Result<String, DaemonError> {
    let mut error = PublicEngineError::new(
        "subscription_cursor_gap",
        format!(
            "Requested event sequence {requested_sequence} is beyond durable sequence {last_sequence}"
        ),
        true,
    );
    error.remediation = Some(format!("Reconnect from sequence {last_sequence}"));
    error.evidence_refs = vec![
        "daemon:subscription-cursor-gap".to_string(),
        "event-store:last-sequence".to_string(),
    ];
    let frame = framer.fail(error, true)?;
    Ok(encode_frame(&frame)?)
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
    let last_sequence = store.last_sequence(run_id).unwrap_or(None).unwrap_or(0);
    if since_sequence > last_sequence {
        lines.push(serde_json::to_string(&subscription_cursor_gap_event(
            &request.id,
            run_id,
            since_sequence,
            last_sequence,
        ))?);
        let stream_metadata = subscription_stream_metadata(&options.workspace, run_id);
        let (mut framer, opened) = StreamFramer::open(
            stream_metadata.stream_id,
            request.id.clone(),
            stream_metadata.project_id,
            stream_metadata.conversation_id,
            Some(run_id.to_string()),
        );
        lines.push(encode_frame(&opened)?);
        lines.push(subscription_stream_gap_frame(
            &mut framer,
            since_sequence,
            last_sequence,
        )?);
        return Ok(lines);
    }

    let Ok(events) = store.replay_since(run_id, since_sequence) else {
        lines.push(serde_json::to_string(&subscription_error_event(
            &request.id,
            run_id,
        ))?);
        return Ok(lines);
    };
    let _ = project_subscription_events_to_timeline(
        &options.workspace,
        run_id,
        &events,
        since_sequence == 0,
    );

    lines.push(serde_json::to_string(&subscription_event(
        Some(request.id.clone()),
        run_id,
        events.len(),
        since_sequence,
    ))?);
    let stream_metadata = subscription_stream_metadata(&options.workspace, run_id);
    let (mut framer, opened) = StreamFramer::open(
        stream_metadata.stream_id,
        request.id.clone(),
        stream_metadata.project_id,
        stream_metadata.conversation_id,
        Some(run_id.to_string()),
    );
    lines.push(encode_frame(&opened)?);
    let mut cursor = since_sequence;
    for event in events {
        cursor = cursor.max(event.sequence);
        append_subscription_execution_event_lines(&mut lines, &mut framer, &event)?;
    }

    let completion_reason = if tail_ms > 0 {
        "tail_complete"
    } else {
        "replay_complete"
    };
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
                lines.push(subscription_stream_failed_frame(&mut framer)?);
                return Ok(lines);
            };
            let _ =
                project_subscription_events_to_timeline(&options.workspace, run_id, &events, false);
            emitted_count += events.len();
            for event in events {
                cursor = cursor.max(event.sequence);
                append_subscription_execution_event_lines(&mut lines, &mut framer, &event)?;
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
    let completed = framer.complete(completion_reason)?;
    lines.push(encode_frame(&completed)?);

    Ok(lines)
}

fn project_subscription_events_to_timeline(
    workspace: &Path,
    run_id: &str,
    events: &[opensks_contracts::ExecutionEventEnvelope],
    rebuild: bool,
) -> Result<(), DaemonError> {
    if events.is_empty() && !rebuild {
        return Ok(());
    }
    if !workspace
        .join(opensks_conversation::CONVERSATION_DB_RELATIVE_PATH)
        .exists()
    {
        return Ok(());
    }
    let repo = opensks_conversation::ConversationRepository::open_workspace(workspace)?;
    let result = if rebuild {
        repo.rebuild_execution_events_into_timeline(run_id, events, timestamp_ms())
    } else {
        repo.project_execution_events_into_timeline(run_id, events, timestamp_ms())
    };
    match result {
        Ok(_) => Ok(()),
        Err(opensks_conversation::ConversationError::NotFound(_)) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn project_run_control_event_to_timeline(
    workspace: &Path,
    run_id: &str,
    event: &ExecutionEventEnvelope,
) -> Result<bool, DaemonError> {
    if !workspace
        .join(opensks_conversation::CONVERSATION_DB_RELATIVE_PATH)
        .exists()
    {
        return Ok(false);
    }
    let repo = opensks_conversation::ConversationRepository::open_workspace(workspace)?;
    match repo.project_execution_events_into_timeline(
        run_id,
        std::slice::from_ref(event),
        timestamp_ms(),
    ) {
        Ok(report) => Ok(report.projected_count > 0
            || report.duplicate_count > 0
            || report.last_sequence > 0),
        Err(opensks_conversation::ConversationError::NotFound(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn project_run_event_journal_to_conversation(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    run_id: &str,
    now_ms: u64,
) -> Result<opensks_conversation::TimelineProjectionReport, String> {
    let store = opensks_event_store::EventStore::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    repo.project_execution_events_into_timeline(run_id, &events, now_ms)
        .map_err(|error| error.to_string())
}

fn append_supervisor_failure_event(
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
    error: &str,
    now_ms: u64,
) -> Result<opensks_contracts::ExecutionEventEnvelope, String> {
    let mut store = opensks_event_store::EventStore::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    let event = opensks_contracts::ExecutionEventEnvelope {
        schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("agent-{}-supervisor-failed-{}", lease.run_id, now_ms),
        run_id: lease.run_id.clone(),
        sequence: 0,
        occurred_at: event_time_from_ms(now_ms),
        actor: "turn-supervisor".to_string(),
        causation_id: Some(lease.turn_id.clone()),
        correlation_id: Some(format!("stream-{}", lease.turn_id)),
        kind: opensks_contracts::EventKind::VerificationFailed,
        payload: serde_json::json!({
            "source": "conversation.supervisor.tick",
            "project_id": lease.project_id,
            "conversation_id": lease.conversation_id,
            "turn_id": lease.turn_id,
            "stream_id": format!("stream-{}", lease.turn_id),
            "state": "failed",
            "reason_code": "turn_supervisor_failed",
            "message": "TurnSupervisor failed.",
            "error_present": !error.is_empty()
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: vec![
            "conversation:turn-supervisor".to_string(),
            "conversation:supervisor-failure".to_string(),
        ],
    };
    store.append_event(event).map_err(|error| error.to_string())
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
    let raw_content = encrypted_raw_prompt_for_turn_start(&options.workspace, turn_request);
    match repo.accept_conversation_turn_with_raw_ciphertext(
        turn_request,
        raw_content.as_ref(),
        timestamp_ms(),
    ) {
        Ok(accepted) => {
            if let Err(error) = record_model_routing_decision(&repo, &accepted.turn_id) {
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
                return Ok(vec![serde_json::to_string(&event)?]);
            }
            if let Err(error) =
                bootstrap_turn_scheduler(&repo, &options.workspace, turn_request, &accepted)
            {
                let mut event = EngineEvent::new(
                    format!(
                        "engine-conversation-turn-start-scheduler-error-{}",
                        request.id
                    ),
                    Some(request.id.clone()),
                    EngineEventType::Error,
                    format!("conversation turn start scheduler bootstrap failed: {error}"),
                    timestamp_ms(),
                );
                event.severity = EventSeverity::Error;
                event.evidence_refs =
                    vec!["daemon:conversation-turn-start-scheduler-error".to_string()];
                event.redacted = true;
                return Ok(vec![serde_json::to_string(&event)?]);
            }
            Ok(vec![serde_json::to_string(&accepted)?])
        }
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

fn encrypted_raw_prompt_for_turn_start(
    workspace: &Path,
    turn_request: &opensks_contracts::ConversationTurnStartRequest,
) -> Option<opensks_conversation::MessageRawContentCiphertext> {
    if !prompt_requires_raw_content(&opensks_conversation::redact_secrets(
        &turn_request.message.text,
    )) {
        return None;
    }
    let identity_text = ensure_raw_prompt_vault_identity_text(workspace).ok()?;
    let ciphertext = opensks_vault::encrypt_bytes_with_identity_text(
        turn_request.message.text.as_bytes(),
        &identity_text,
    )
    .ok()?;
    Some(opensks_conversation::MessageRawContentCiphertext {
        ciphertext,
        nonce: b"age-x25519-keychain-v1".to_vec(),
    })
}

fn ensure_raw_prompt_vault_identity_text(workspace: &Path) -> Result<String, String> {
    match resolve_raw_prompt_vault_identity_text(workspace) {
        Ok(identity_text) => Ok(identity_text),
        Err(_) => provision_raw_prompt_vault_identity_text(workspace),
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

fn bootstrap_turn_scheduler(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    turn_request: &opensks_contracts::ConversationTurnStartRequest,
    accepted: &opensks_contracts::ConversationTurnAccepted,
) -> Result<opensks_scheduler::ConversationTurnSchedulerBootstrap, DaemonError> {
    let settings_json = repo
        .turn_effective_settings_json(&accepted.turn_id)?
        .ok_or_else(|| {
            DaemonError::Io(std::io::Error::other(
                "accepted turn has no settings snapshot",
            ))
        })?;
    let settings: opensks_contracts::ConversationTurnSettings =
        serde_json::from_str(&settings_json)?;
    let settings_digest = repo
        .turn_settings_digest(&accepted.turn_id)?
        .ok_or_else(|| {
            DaemonError::Io(std::io::Error::other(
                "accepted turn has no settings digest",
            ))
        })?;
    let conversation_digest = repo.get_digest(&turn_request.conversation_id)?;
    let context_pack_ref = build_turn_context_pack_ref(
        workspace,
        accepted,
        &settings,
        &turn_request.context.refs,
        conversation_digest,
    )?;
    let resource_limits = turn_scheduler_resource_limits_from_registry(workspace, &settings);
    let role_plan = turn_scheduler_role_plan_for_settings(workspace, &settings);
    let objective_plan =
        turn_scheduler_objective_plan(workspace, &settings, turn_request, accepted).map_err(
            |error| {
                DaemonError::Io(std::io::Error::other(format!(
                    "objective planner bootstrap failed: {error}"
                )))
            },
        )?;
    let mut store = opensks_event_store::EventStore::open_workspace(workspace)
        .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
    opensks_scheduler::bootstrap_conversation_turn_scheduler(
        &mut store,
        opensks_scheduler::ConversationTurnSchedulerInput {
            run_id: &accepted.run_id,
            turn_id: &accepted.turn_id,
            project_id: &turn_request.project_id,
            conversation_id: &turn_request.conversation_id,
            settings: &settings,
            settings_digest: &settings_digest,
            context_pack_ref: Some(&context_pack_ref),
            resource_limits,
            role_plan,
            objective_plan,
            now_ms: timestamp_ms(),
        },
    )
    .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))
}

fn turn_scheduler_objective_plan(
    workspace: &Path,
    settings: &opensks_contracts::ConversationTurnSettings,
    turn_request: &opensks_contracts::ConversationTurnStartRequest,
    accepted: &opensks_contracts::ConversationTurnAccepted,
) -> Result<Option<opensks_scheduler::ConversationTurnObjectivePlan>, String> {
    build_turn_objective_plan(
        workspace,
        settings,
        &accepted.run_id,
        &turn_request.message.text,
        true,
    )
}

fn claimed_turn_objective_plan(
    workspace: &Path,
    settings: &opensks_contracts::ConversationTurnSettings,
    run_id: &str,
    objective_text: &str,
) -> Result<Option<opensks_scheduler::ConversationTurnObjectivePlan>, String> {
    let Some(mut plan) =
        build_turn_objective_plan(workspace, settings, run_id, objective_text, false)?
    else {
        return Ok(None);
    };
    if !plan
        .work_items
        .iter()
        .all(is_claimed_turn_objective_runtime_supported)
    {
        return Ok(None);
    }
    plan.work_items
        .retain(is_claimed_turn_objective_runtime_dispatchable);
    if plan.work_items.is_empty() {
        return Ok(None);
    }
    push_unique_string(
        &mut plan.evidence_refs,
        "daemon:objective-plan-child-runtime".to_string(),
    );
    Ok(Some(plan))
}

fn build_turn_objective_plan(
    workspace: &Path,
    settings: &opensks_contracts::ConversationTurnSettings,
    run_id: &str,
    objective_text: &str,
    allow_live_planner: bool,
) -> Result<Option<opensks_scheduler::ConversationTurnObjectivePlan>, String> {
    if settings.pipeline_id != "objective-planner" {
        return Ok(None);
    }
    let mut plan_request = opensks_contracts::ObjectivePlanRequest::new(objective_text.to_string());
    plan_request.max_parallelism = settings.max_parallelism.max(1);
    plan_request.role_count = settings.max_parallelism.max(1);
    plan_request.evidence_refs = vec![
        "daemon:conversation-turn-objective-planner-bootstrap".to_string(),
        "conversation:turn-accepted".to_string(),
    ];
    let live_planner = if allow_live_planner {
        apply_live_objective_planner_directive(workspace, settings, &mut plan_request)?
    } else {
        None
    };
    let mut planned = opensks_graph::plan_graph_from_objective(&plan_request);
    if let Some(live_planner) = live_planner {
        planned.receipt.source = "model_authored_objective_planner".to_string();
        planned.receipt.planner_provider_id = Some(live_planner.provider_id);
        planned.receipt.planner_model_id = Some(live_planner.model_id);
        planned.receipt.planner_response_hash = Some(live_planner.response_hash);
        planned.receipt.planner_response_bytes = Some(live_planner.response_bytes);
        push_unique_string(
            &mut planned.receipt.evidence_refs,
            "daemon:objective-plan-live-model-planner".to_string(),
        );
        push_unique_string(
            &mut planned.receipt.evidence_refs,
            "provider:role-routing".to_string(),
        );
    }
    let run_plan = opensks_engine::plan_graph_for_scheduler(run_id, &planned.graph)
        .map_err(|error| error.to_string())?;
    let artifact_refs = write_objective_plan_artifacts(workspace, run_id, &mut planned)?;
    let mut evidence_refs = planned.receipt.evidence_refs.clone();
    if !evidence_refs
        .iter()
        .any(|evidence| evidence == "daemon:conversation-turn-objective-planner-bootstrap")
    {
        evidence_refs.push("daemon:conversation-turn-objective-planner-bootstrap".to_string());
    }
    evidence_refs.push("daemon:objective-plan-artifact".to_string());
    evidence_refs.push("engine:scheduler-requirement-propagation".to_string());
    Ok(Some(opensks_scheduler::ConversationTurnObjectivePlan {
        graph_id: planned.receipt.graph_id,
        plan_hash: planned.receipt.plan_hash,
        source: planned.receipt.source,
        graph_ref: Some(artifact_refs.graph_ref),
        compiled_plan_ref: Some(artifact_refs.compiled_plan_ref),
        receipt_ref: Some(artifact_refs.receipt_ref),
        work_items: run_plan.work_items,
        evidence_refs,
    }))
}

#[derive(Debug, Clone)]
struct LiveObjectivePlannerMetadata {
    provider_id: String,
    model_id: String,
    response_hash: String,
    response_bytes: usize,
}

#[derive(Debug, Clone, Default)]
struct ObjectivePlannerDirective {
    max_parallelism: Option<u32>,
    role_count: Option<u32>,
    include_image_lane: Option<bool>,
    include_research_lane: Option<bool>,
}

fn apply_live_objective_planner_directive(
    workspace: &Path,
    settings: &opensks_contracts::ConversationTurnSettings,
    plan_request: &mut opensks_contracts::ObjectivePlanRequest,
) -> Result<Option<LiveObjectivePlannerMetadata>, String> {
    let Some(dispatch) = prepare_objective_planner_provider_dispatch(workspace)? else {
        return Ok(None);
    };
    let completer = opensks_adapter::OpenAiCompatibleChatCompleter::new(
        dispatch.connection.endpoint.base_url.clone(),
        dispatch.bearer_token.clone(),
    )
    .map_err(|_error| "objective_planner_model_call_failed".to_string())?;
    let response = opensks_adapter::ChatCompleter::complete(
        &completer,
        &objective_planner_model_call_body(
            &dispatch.model.remote_model_id,
            max_output_tokens_for_model(&dispatch.model).min(256),
            plan_request,
            provider_reasoning_effort_for_provider(
                dispatch.connection.kind,
                settings.reasoning_effort,
            ),
        ),
    )
    .map_err(|_error| "objective_planner_model_call_failed".to_string())?;
    let content = response
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "objective_planner_model_call_returned_no_text".to_string())?;
    let directive = parse_objective_planner_directive(content)?;
    apply_objective_planner_directive(settings, plan_request, directive);
    push_unique_string(
        &mut plan_request.evidence_refs,
        "daemon:objective-plan-live-model-planner".to_string(),
    );
    push_unique_string(
        &mut plan_request.evidence_refs,
        "provider:role-routing".to_string(),
    );
    Ok(Some(LiveObjectivePlannerMetadata {
        provider_id: dispatch.connection.id,
        model_id: dispatch.model.id,
        response_hash: stable_text_digest(content),
        response_bytes: content.len(),
    }))
}

fn prepare_objective_planner_provider_dispatch(
    workspace: &Path,
) -> Result<Option<RoleProviderDispatchTarget>, String> {
    let provider_repo = opensks_provider::ProviderRepository::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    let registry = opensks_provider::model_registry_from_repository(&provider_repo)
        .map_err(|error| error.to_string())?;
    let plan = registry.route_roles(&opensks_provider::RoleRoutingRequest::with_roles(
        "conversation-turn-objective-planner-live-model",
        vec![opensks_contracts::ModelRole::Planning],
    ));
    let Some(assignment) = plan
        .assignments
        .iter()
        .find(|assignment| assignment.role == opensks_contracts::ModelRole::Planning)
    else {
        return Ok(None);
    };
    if !assignment.decision.status.has_resolved_model() {
        return Ok(None);
    }
    let Some(model_id) = assignment.decision.selected_model_id.as_deref() else {
        return Ok(None);
    };
    let model = provider_repo
        .get_model(model_id)
        .map_err(|error| error.to_string())?;
    if !model.enabled {
        return Err("objective planner selected model is disabled".to_string());
    }
    if matches!(
        model.health,
        opensks_contracts::HealthState::Unavailable | opensks_contracts::HealthState::OpenCircuit
    ) {
        return Err("objective planner selected model health is unavailable".to_string());
    }
    let connection = provider_repo
        .get_connection(&model.provider_id)
        .map_err(|error| error.to_string())?;
    if !connection.enabled {
        return Err("objective planner selected provider is disabled".to_string());
    }
    if matches!(
        connection.health.state,
        opensks_contracts::HealthState::Unavailable | opensks_contracts::HealthState::OpenCircuit
    ) || connection.health.circuit_open
    {
        return Err("objective planner selected provider health is unavailable".to_string());
    }
    if !supports_openai_compatible_dispatch(connection.kind) {
        return Err(format!(
            "provider kind `{:?}` is not supported by objective planner dispatch",
            connection.kind
        ));
    }
    let bearer_token = resolve_provider_secret_for_dispatch(&connection.auth)?;
    Ok(Some(RoleProviderDispatchTarget {
        connection,
        model,
        bearer_token,
    }))
}

fn objective_planner_model_call_body(
    remote_model_id: &str,
    max_tokens: u32,
    request: &opensks_contracts::ObjectivePlanRequest,
    reasoning_effort: ProviderReasoningEffort,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": remote_model_id,
        "max_tokens": max_tokens.clamp(1, 256),
        "temperature": 0,
        "messages": [
            {
                "role": "system",
                "content": "You are the OpenSKS objective planner. Return JSON only. Allowed keys: max_parallelism, role_count, include_image_lane, include_research_lane. For this runtime, include_image_lane and include_research_lane must be false. Do not include raw secrets, file contents, prose, or markdown."
            },
            {
                "role": "user",
                "content": format!(
                    "Objective:\n{}\n\nReturn a compact JSON directive for the OpenSKS planner. max_parallelism and role_count must be positive integers. include_image_lane/include_research_lane must be booleans and currently false.",
                    request.objective
                )
            }
        ]
    });
    add_provider_reasoning_effort(&mut body, reasoning_effort);
    body
}

fn parse_objective_planner_directive(content: &str) -> Result<ObjectivePlannerDirective, String> {
    let value: serde_json::Value = serde_json::from_str(
        extract_json_object_text(content)
            .ok_or_else(|| "objective_planner_model_call_returned_invalid_json".to_string())?,
    )
    .map_err(|_| "objective_planner_model_call_returned_invalid_json".to_string())?;
    let directive_value = value
        .get("objective_plan")
        .or_else(|| value.get("plan"))
        .unwrap_or(&value)
        .clone();
    objective_planner_directive_from_value(&directive_value)
}

fn objective_planner_directive_from_value(
    value: &serde_json::Value,
) -> Result<ObjectivePlannerDirective, String> {
    Ok(ObjectivePlannerDirective {
        max_parallelism: optional_u32_directive_field(value, "max_parallelism")?,
        role_count: optional_u32_directive_field(value, "role_count")?,
        include_image_lane: optional_bool_directive_field(value, "include_image_lane")?,
        include_research_lane: optional_bool_directive_field(value, "include_research_lane")?,
    })
}

fn optional_u32_directive_field(
    value: &serde_json::Value,
    field: &str,
) -> Result<Option<u32>, String> {
    let Some(raw) = value.get(field) else {
        return Ok(None);
    };
    let Some(number) = raw.as_u64() else {
        return Err("objective_planner_model_call_returned_invalid_directive".to_string());
    };
    Ok(Some(number.min(u64::from(u32::MAX)) as u32))
}

fn optional_bool_directive_field(
    value: &serde_json::Value,
    field: &str,
) -> Result<Option<bool>, String> {
    let Some(raw) = value.get(field) else {
        return Ok(None);
    };
    raw.as_bool()
        .map(Some)
        .ok_or_else(|| "objective_planner_model_call_returned_invalid_directive".to_string())
}

fn extract_json_object_text(content: &str) -> Option<&str> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end > start).then_some(&trimmed[start..=end])
}

fn apply_objective_planner_directive(
    settings: &opensks_contracts::ConversationTurnSettings,
    request: &mut opensks_contracts::ObjectivePlanRequest,
    directive: ObjectivePlannerDirective,
) {
    let max_allowed = settings.max_parallelism.max(1);
    if let Some(max_parallelism) = directive.max_parallelism {
        request.max_parallelism = max_parallelism.clamp(1, max_allowed);
    }
    if let Some(role_count) = directive.role_count {
        request.role_count = role_count.clamp(1, max_allowed);
    }
    if directive.include_image_lane.unwrap_or(false)
        || directive.include_research_lane.unwrap_or(false)
    {
        push_unique_string(
            &mut request.evidence_refs,
            "daemon:objective-plan-optional-lanes-clamped".to_string(),
        );
    }
    // Keep model-suggested optional lanes off until claimed-turn executors exist for them.
    request.include_image_lane = false;
    request.include_research_lane = false;
    request.require_git_worktree = true;
    request.require_integration_approval = true;
}

fn is_claimed_turn_objective_runtime_supported(
    item: &opensks_contracts::SchedulerWorkItem,
) -> bool {
    matches!(
        item.node_id.as_str(),
        "goal"
            | "decompose"
            | "worktree"
            | "role_router"
            | "workers"
            | "verifier"
            | "apply"
            | "seal"
    )
}

fn is_claimed_turn_objective_runtime_dispatchable(
    item: &opensks_contracts::SchedulerWorkItem,
) -> bool {
    matches!(
        item.node_id.as_str(),
        "goal"
            | "decompose"
            | "worktree"
            | "role_router"
            | "workers"
            | "verifier"
            | "apply"
            | "seal"
    )
}

#[derive(Debug, Clone)]
struct ObjectivePlanArtifactRefs {
    graph_ref: String,
    compiled_plan_ref: String,
    receipt_ref: String,
}

fn write_objective_plan_artifacts(
    workspace: &Path,
    run_id: &str,
    planned: &mut opensks_graph::ObjectiveGraphPlan,
) -> Result<ObjectivePlanArtifactRefs, String> {
    let safe_run_id = safe_artifact_segment(run_id);
    let relative_dir = PathBuf::from(".opensks")
        .join("runtime")
        .join("objective-plans")
        .join(safe_run_id);
    let graph_relative = relative_dir.join("graph.json");
    let compiled_plan_relative = relative_dir.join("compiled-plan.json");
    let receipt_relative = relative_dir.join("receipt.json");
    let graph_ref = artifact_ref(&graph_relative);
    let compiled_plan_ref = artifact_ref(&compiled_plan_relative);
    let receipt_ref = artifact_ref(&receipt_relative);
    planned.receipt.graph_ref = Some(graph_ref.clone());
    planned.receipt.compiled_plan_ref = Some(compiled_plan_ref.clone());
    let graph_body =
        serde_json::to_vec_pretty(&planned.graph).map_err(|error| error.to_string())?;
    let compiled_plan_body =
        serde_json::to_vec_pretty(&planned.compiled_plan).map_err(|error| error.to_string())?;
    let receipt_body =
        serde_json::to_vec_pretty(&planned.receipt).map_err(|error| error.to_string())?;
    write_daemon_json_artifact_atomic(&workspace.join(&graph_relative), graph_body)
        .map_err(|error| error.to_string())?;
    write_daemon_json_artifact_atomic(&workspace.join(&compiled_plan_relative), compiled_plan_body)
        .map_err(|error| error.to_string())?;
    write_daemon_json_artifact_atomic(&workspace.join(&receipt_relative), receipt_body)
        .map_err(|error| error.to_string())?;
    Ok(ObjectivePlanArtifactRefs {
        graph_ref,
        compiled_plan_ref,
        receipt_ref,
    })
}

fn turn_scheduler_resource_limits_from_registry(
    workspace: &Path,
    settings: &opensks_contracts::ConversationTurnSettings,
) -> Option<opensks_scheduler::ConversationTurnSchedulerResourceLimits> {
    let model_id = settings.model.model_id.as_deref()?;
    let repo = opensks_provider::ProviderRepository::open_workspace(workspace).ok()?;
    let model = repo.get_model(model_id).ok()?;
    let provider = repo.get_connection(&model.provider_id).ok()?;
    let provider_max_workers = provider.concurrency.max_concurrent_requests.max(1);
    let per_model_max_workers = model
        .limits
        .max_concurrency
        .unwrap_or(provider_max_workers)
        .max(1)
        .min(provider_max_workers);
    Some(opensks_scheduler::ConversationTurnSchedulerResourceLimits {
        provider_max_workers,
        per_provider_max_workers: provider_max_workers,
        per_model_max_workers,
    })
}

fn turn_scheduler_role_plan_from_registry(
    workspace: &Path,
) -> Option<opensks_scheduler::ConversationTurnSchedulerRolePlan> {
    let repo = opensks_provider::ProviderRepository::open_workspace(workspace).ok()?;
    let registry = opensks_provider::model_registry_from_repository(&repo).ok()?;
    let plan = registry.route_roles(
        &opensks_provider::RoleRoutingRequest::hyperparallel_default(
            "conversation-turn-hyperparallel-roles",
        ),
    );
    Some(scheduler_role_plan_from_provider_plan(&plan))
}

fn turn_scheduler_role_plan_for_settings(
    workspace: &Path,
    settings: &opensks_contracts::ConversationTurnSettings,
) -> Option<opensks_scheduler::ConversationTurnSchedulerRolePlan> {
    if settings.pipeline_id == "objective-planner" {
        return None;
    }
    turn_scheduler_role_plan_from_registry(workspace)
}

fn scheduler_role_plan_from_provider_plan(
    plan: &opensks_provider::RoleRoutingPlan,
) -> opensks_scheduler::ConversationTurnSchedulerRolePlan {
    let mut distinct_model_ids = BTreeSet::new();
    let mut reused_model_count = 0_u32;
    let mut blocked_role_count = 0_u32;
    let assignments = plan
        .assignments
        .iter()
        .map(|assignment| {
            if let Some(model_id) = assignment.decision.selected_model_id.as_ref() {
                distinct_model_ids.insert(model_id.clone());
            }
            if assignment.reused_model {
                reused_model_count += 1;
            }
            if !assignment.decision.status.has_resolved_model() {
                blocked_role_count += 1;
            }
            opensks_scheduler::ConversationTurnSchedulerRoleAssignment {
                role: assignment.role.clone(),
                status: assignment.decision.status.clone(),
                selected_model_id: assignment.decision.selected_model_id.clone(),
                provider_id: assignment
                    .decision
                    .route_receipt
                    .as_ref()
                    .and_then(|receipt| receipt.provider_id.clone()),
                reason_code: assignment.decision.reason_code.clone(),
                reused_model: assignment.reused_model,
            }
        })
        .collect();
    opensks_scheduler::ConversationTurnSchedulerRolePlan {
        reason_code: plan.reason_code.clone(),
        assignments,
        distinct_model_count: distinct_model_ids.len() as u32,
        reused_model_count,
        blocked_role_count,
    }
}

fn build_turn_context_pack_ref(
    workspace: &Path,
    accepted: &opensks_contracts::ConversationTurnAccepted,
    settings: &opensks_contracts::ConversationTurnSettings,
    context_refs: &[String],
    conversation_digest: Option<opensks_contracts::ConversationDigest>,
) -> Result<String, DaemonError> {
    let id = format!("turn-context-{}", accepted.turn_id);
    let token_budget = settings
        .token_budget
        .map(|budget| budget.min(32_000) as u32)
        .unwrap_or(2_048)
        .clamp(1, 32_000);
    let mut pack = opensks_context::pack_workspace_records_with_turn_context(
        workspace,
        &id,
        token_budget,
        context_refs,
    )
    .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
    if let Some(digest) = conversation_digest {
        opensks_context::add_conversation_summary(&mut pack, digest);
    }
    let path = opensks_context::write_context_pack(workspace, &pack)
        .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
    let relative = path.strip_prefix(workspace).map_err(|error| {
        DaemonError::Io(std::io::Error::other(format!(
            "context pack path escaped workspace: {error}"
        )))
    })?;
    Ok(artifact_ref(relative))
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
            let executed =
                execute_claimed_conversation_turn(&repo, &options.workspace, &lease, lease_ttl_ms);
            let claimed_json = serde_json::json!({
                "turn_id": lease.turn_id,
                "run_id": lease.run_id,
                "project_id": lease.project_id,
                "conversation_id": lease.conversation_id,
                "assistant_message_id": lease.assistant_message_id,
                "lease_owner": lease.lease_owner,
                "lease_expires_at_ms": lease.lease_expires_at_ms,
                "fencing_token": lease.fencing_token,
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
    lease_ttl_ms: u64,
) -> serde_json::Value {
    match execute_claimed_conversation_turn_inner(repo, workspace, lease, lease_ttl_ms) {
        Ok(value) => value,
        Err(error) => {
            let now_ms = timestamp_ms();
            let _ = repo.set_message_content(
                &lease.assistant_message_id,
                &format!("TurnSupervisor failed: {error}"),
                opensks_contracts::MessageState::Failed,
                now_ms,
            );
            let last_event_sequence =
                append_supervisor_failure_event(workspace, lease, &error, now_ms)
                    .and_then(|_| {
                        project_run_event_journal_to_conversation(
                            repo,
                            workspace,
                            &lease.run_id,
                            now_ms,
                        )
                    })
                    .and_then(|_| {
                        repo.finish_turn_supervisor_lease_after_projection(lease, now_ms)
                            .map_err(|error| error.to_string())
                    })
                    .map(|finished| finished.last_event_sequence)
                    .unwrap_or_else(|_| {
                        let _ =
                            repo.finish_turn_supervisor_lease(lease, "failed", 0, "failed", now_ms);
                        0
                    });
            serde_json::json!({
                "status": "failed",
                "run_state": "failed",
                "error": error,
                "last_event_sequence": last_event_sequence,
            })
        }
    }
}

fn execute_claimed_conversation_turn_inner(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
    lease_ttl_ms: u64,
) -> Result<serde_json::Value, String> {
    let now_ms = timestamp_ms();
    let settings: opensks_contracts::ConversationTurnSettings =
        serde_json::from_str(&lease.effective_settings_json).map_err(|error| error.to_string())?;
    if let Some(cancellation) = observe_run_cancellation(workspace, &lease.run_id)? {
        return finish_cancelled_claimed_conversation_turn(
            repo,
            workspace,
            lease,
            &cancellation,
            now_ms,
        );
    }
    if let Some(recovered) =
        recover_candidate_ready_claimed_conversation_turn(repo, workspace, lease, now_ms)?
    {
        return Ok(recovered);
    }
    let routing_decision = resolve_claimed_turn_routing(repo, workspace, lease, &settings, now_ms)
        .map_err(|error| error.to_string())?;
    let stored_prompt = repo
        .turn_user_message_text(&lease.turn_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "accepted turn has no durable user prompt".to_string())?;
    let prompt_resolution =
        resolve_execution_prompt(repo, workspace, &lease.turn_id, &stored_prompt)?;
    let prompt = prompt_resolution.prompt;
    let selected_model_id = routing_decision
        .status
        .has_resolved_model()
        .then(|| routing_decision.selected_model_id.clone())
        .flatten();
    let explicit_local_test = opensks_adapter::LocalTestInstruction::from_prompt(&prompt).is_some();
    let should_isolate_execution = matches!(
        settings.execution_mode,
        opensks_contracts::ExecutionMode::Worktree
    ) && (selected_model_id.is_some() || explicit_local_test);
    let execution_workspace =
        prepare_turn_execution_workspace(workspace, lease, should_isolate_execution)
            .map_err(|error| format!("prepare execution workspace: {error}"))?;
    let cancellation_token = Arc::new(AtomicBool::new(false));
    let request = opensks_adapter::AgentRunRequest {
        workspace: execution_workspace.path.clone(),
        project_id: lease.project_id.clone(),
        conversation_id: lease.conversation_id.clone(),
        turn_id: lease.turn_id.clone(),
        run_id: lease.run_id.clone(),
        stream_id: format!("stream-{}", lease.turn_id),
        patch_lease: None,
        cancellation_token: Some(Arc::clone(&cancellation_token)),
        now_ms,
        prompt: prompt.clone(),
    };
    let sink = DaemonAgentEventSink::open(workspace).map_err(|error| error.to_string())?;
    sink.emit_run_started(&request)
        .map_err(|error| error.to_string())?;
    if let Some(isolation) = execution_workspace.isolation.as_ref() {
        opensks_adapter::AgentEventSink::emit(
            &sink,
            execution_workspace_prepared_agent_event(&request, isolation),
        );
    }
    if prompt_resolution.raw_prompt_restored {
        opensks_adapter::AgentEventSink::emit(
            &sink,
            raw_prompt_restored_after_redaction_agent_event(
                &request,
                prompt_resolution.raw_ciphertext_bytes,
                prompt.len(),
            ),
        );
    }

    let agentic_config = opensks_adapter::AgenticConfig::for_turn_settings(&settings);
    let execution = ClaimedTurnExecution {
        repo,
        workspace,
        lease,
        settings: &settings,
        execution_workspace: &execution_workspace,
        initial_routing_decision: &routing_decision,
        explicit_local_test,
        request: &request,
        sink: &sink,
        prompt: &prompt,
        agentic_config: &agentic_config,
        now_ms,
    };
    let lease_heartbeat = TurnLeaseHeartbeatGuard::start(
        repo,
        workspace,
        lease,
        lease_ttl_ms,
        Arc::clone(&cancellation_token),
    )
    .map_err(|error| format!("start turn lease heartbeat: {error}"))?;
    let (outcome, mut final_routing_decision) = if prompt_requires_raw_content(&prompt) {
        opensks_adapter::AgentEventSink::emit(
            &sink,
            raw_prompt_unavailable_after_redaction_agent_event(&request),
        );
        (
            opensks_adapter::AgentRunOutcome {
                assistant_text: "Cannot execute this turn because the durable prompt is redacted and the raw prompt vault is unavailable.".to_string(),
                patches: vec![],
                apply_results: vec![],
                final_state: opensks_contracts::projection::RunProjectionState::Failed,
            },
            routing_decision.clone(),
        )
    } else {
        dispatch_claimed_turn_via_scheduler(execution)?
    };
    let cancellation_after_dispatch = observe_run_cancellation(workspace, &lease.run_id)?;
    let outcome = if cancellation_after_dispatch.is_some() {
        opensks_adapter::AgentRunOutcome {
            assistant_text: "Turn cancelled before integration.".to_string(),
            patches: outcome.patches,
            apply_results: outcome.apply_results,
            final_state: opensks_contracts::projection::RunProjectionState::Cancelled,
        }
    } else {
        outcome
    };
    if let Some(decision_json) = repo
        .turn_model_routing_decision_json(&lease.turn_id)
        .map_err(|error| error.to_string())?
    {
        final_routing_decision =
            serde_json::from_str(&decision_json).map_err(|error| error.to_string())?;
    }
    let final_selected_model_id = final_routing_decision
        .status
        .has_resolved_model()
        .then(|| final_routing_decision.selected_model_id.clone())
        .flatten();
    let integration_candidate = if cancellation_after_dispatch.is_some() {
        None
    } else {
        write_integration_candidate(
            workspace,
            lease,
            &settings,
            &execution_workspace,
            &outcome,
            now_ms,
        )?
    };
    if let Some(candidate) = integration_candidate.as_ref() {
        opensks_adapter::AgentEventSink::emit(
            &sink,
            integration_candidate_agent_event(&request, &candidate.receipt),
        );
    }
    if outcome.final_state == opensks_contracts::projection::RunProjectionState::Completed {
        sink.emit_run_completed(
            &request,
            final_selected_model_id.as_deref(),
            integration_candidate
                .as_ref()
                .map(|candidate| &candidate.receipt),
            timestamp_ms(),
        )
        .map_err(|error| error.to_string())?;
    } else if outcome.final_state == opensks_contracts::projection::RunProjectionState::Failed {
        sink.emit_run_failed(&request, timestamp_ms())
            .map_err(|error| error.to_string())?;
    }
    sink.finish(&lease.run_id)
        .map_err(|error| error.to_string())?;
    project_run_event_journal_to_conversation(repo, workspace, &lease.run_id, timestamp_ms())?;
    let (_run_state, assistant_state) = match outcome.final_state {
        opensks_contracts::projection::RunProjectionState::Completed => {
            ("completed", opensks_contracts::MessageState::Complete)
        }
        opensks_contracts::projection::RunProjectionState::Failed => {
            ("failed", opensks_contracts::MessageState::Failed)
        }
        opensks_contracts::projection::RunProjectionState::Cancelled => {
            ("cancelled", opensks_contracts::MessageState::Cancelled)
        }
        opensks_contracts::projection::RunProjectionState::Paused => {
            ("paused", opensks_contracts::MessageState::Streaming)
        }
        opensks_contracts::projection::RunProjectionState::Queued
        | opensks_contracts::projection::RunProjectionState::Running => {
            ("running", opensks_contracts::MessageState::Streaming)
        }
    };
    let assistant_text = integration_candidate
        .as_ref()
        .map(|candidate| candidate.assistant_text.clone())
        .unwrap_or_else(|| outcome.assistant_text.clone());
    repo.set_message_content(
        &lease.assistant_message_id,
        &assistant_text,
        assistant_state,
        now_ms,
    )
    .map_err(|error| error.to_string())?;
    let finished = repo
        .finish_turn_supervisor_lease_after_projection(lease, now_ms)
        .map_err(|error| error.to_string())?;
    let last_event_sequence = finished.last_event_sequence;
    let run_state = finished.state;

    Ok(serde_json::json!({
        "status": "executed",
        "run_state": run_state,
        "model_routing_status": final_routing_decision.status,
        "model_routing_reason_code": final_routing_decision.reason_code,
        "selected_model_id": final_selected_model_id,
        "execution_mode": settings.execution_mode,
        "execution_workspace_mode": execution_workspace.mode_label(),
        "execution_isolated": execution_workspace.isolation.is_some(),
        "execution_isolation_reason_code": execution_workspace.reason_code(),
        "lease_heartbeat": lease_heartbeat.status_json(),
        "cancellation": cancellation_after_dispatch
            .as_ref()
            .map(RunCancellationObservation::status_json),
        "integration_state": integration_candidate
            .as_ref()
            .map(|candidate| candidate.receipt.state.as_str())
            .unwrap_or("not_required"),
        "integration_candidate_ref": integration_candidate
            .as_ref()
            .map(|candidate| candidate.receipt.receipt_ref.as_str()),
        "integration_patch_ref": integration_candidate
            .as_ref()
            .map(|candidate| candidate.receipt.patch_ref.as_str()),
        "integration_selection_ref": integration_candidate
            .as_ref()
            .and_then(|candidate| candidate.receipt.selection_ref.as_deref()),
        "integration_planned_verifier_count": integration_candidate
            .as_ref()
            .map(|candidate| candidate.receipt.planned_verifier_count)
            .unwrap_or(0),
        "integration_target_count": integration_candidate
            .as_ref()
            .map(|candidate| candidate.receipt.target_paths.len())
            .unwrap_or(0),
        "assistant_message_id": lease.assistant_message_id,
        "last_event_sequence": last_event_sequence,
        "patch_count": outcome.patches.len(),
        "apply_result_count": outcome.apply_results.len(),
    }))
}

fn recover_candidate_ready_claimed_conversation_turn(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
    now_ms: u64,
) -> Result<Option<serde_json::Value>, String> {
    let candidate_dir = workspace
        .join(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(&lease.run_id);
    let candidate_raw = match std::fs::read_to_string(candidate_dir.join("candidate.json")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let candidate: IntegrationCandidateReceipt =
        serde_json::from_str(&candidate_raw).map_err(|error| error.to_string())?;
    if candidate.schema != opensks_contracts::INTEGRATION_CANDIDATE_RECEIPT_SCHEMA
        || candidate.run_id != lease.run_id
        || candidate.state != "candidate_ready"
        || candidate.target_paths.is_empty()
        || !candidate_dir.join("candidate.patch").is_file()
        || !candidate_dir.join("selection.json").is_file()
    {
        return Ok(None);
    }

    project_run_event_journal_to_conversation(repo, workspace, &lease.run_id, now_ms)?;
    let Some(state) = repo
        .run_projection_state(&lease.run_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    if state != "completed" {
        return Ok(None);
    }

    let recovery_lines = worktree_isolation_recovery_lines(
        &format!("turn-supervisor-candidate-ready-recovery-{}", lease.run_id),
        workspace,
        &lease.run_id,
    )
    .map_err(|error| error.to_string())?;
    let recovery = recovery_lines
        .iter()
        .filter_map(|line| {
            serde_json::from_str::<opensks_contracts::WorktreeIsolationRecoveryReceipt>(line).ok()
        })
        .next();
    project_run_event_journal_to_conversation(repo, workspace, &lease.run_id, now_ms)?;
    let assistant_text = format!(
        "Recovered isolated change candidate for {}. The main workspace was not modified; integration is pending verification and approval.",
        candidate.target_paths.join(", ")
    );
    repo.set_message_content(
        &lease.assistant_message_id,
        &assistant_text,
        opensks_contracts::MessageState::Complete,
        now_ms,
    )
    .map_err(|error| error.to_string())?;
    let finished = repo
        .finish_turn_supervisor_lease_after_projection(lease, now_ms)
        .map_err(|error| error.to_string())?;
    Ok(Some(serde_json::json!({
        "status": "recovered_candidate_ready",
        "run_state": finished.state,
        "execution_skipped": true,
        "integration_state": candidate.state,
        "integration_candidate_ref": candidate.receipt_ref,
        "integration_patch_ref": candidate.patch_ref,
        "integration_selection_ref": candidate.selection_ref,
        "integration_planned_verifier_count": candidate.planned_verifier_count,
        "integration_target_count": candidate.target_paths.len(),
        "worktree_recovery_state": recovery.as_ref().map(|receipt| receipt.state.as_str()),
        "worktree_recovery_ref": recovery.as_ref().map(|receipt| receipt.recovery_ref.as_str()),
        "worktree_recovered_count": recovery.as_ref().map(|receipt| receipt.recovered_count),
        "assistant_message_id": lease.assistant_message_id,
        "last_event_sequence": finished.last_event_sequence,
        "patch_count": 0,
        "apply_result_count": 0,
    })))
}

#[derive(Debug, Clone)]
struct RunCancellationObservation {
    sequence: u64,
    reason_code: String,
}

impl RunCancellationObservation {
    fn status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "observed": true,
            "sequence": self.sequence,
            "reason_code": self.reason_code,
        })
    }
}

fn observe_run_cancellation(
    workspace: &Path,
    run_id: &str,
) -> Result<Option<RunCancellationObservation>, String> {
    let store = opensks_event_store::EventStore::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    Ok(events
        .into_iter()
        .filter(|event| event.kind == opensks_contracts::EventKind::RunCancelled)
        .max_by_key(|event| event.sequence)
        .map(|event| RunCancellationObservation {
            sequence: event.sequence,
            reason_code: event
                .payload
                .get("reason_code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("cancelled_by_user")
                .to_string(),
        }))
}

fn finish_cancelled_claimed_conversation_turn(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
    cancellation: &RunCancellationObservation,
    now_ms: u64,
) -> Result<serde_json::Value, String> {
    repo.set_message_content(
        &lease.assistant_message_id,
        "Turn cancelled before execution.",
        opensks_contracts::MessageState::Cancelled,
        now_ms,
    )
    .map_err(|error| error.to_string())?;
    project_run_event_journal_to_conversation(repo, workspace, &lease.run_id, now_ms)?;
    let finished = repo
        .finish_turn_supervisor_lease_after_projection(lease, now_ms)
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({
        "status": "cancelled",
        "run_state": finished.state,
        "execution_skipped": true,
        "cancellation": cancellation.status_json(),
        "assistant_message_id": lease.assistant_message_id,
        "last_event_sequence": finished.last_event_sequence,
        "patch_count": 0,
        "apply_result_count": 0,
    }))
}

#[derive(Debug, Clone)]
struct TurnLeaseHeartbeatStatus {
    started: bool,
    initial_heartbeat_ok: bool,
    interval_ms: u64,
    fencing_token: u64,
}

struct TurnLeaseHeartbeatGuard {
    stop_tx: Option<mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
    cancellation_observed: Arc<AtomicBool>,
    status: TurnLeaseHeartbeatStatus,
}

impl TurnLeaseHeartbeatGuard {
    fn start(
        repo: &opensks_conversation::ConversationRepository,
        workspace: &Path,
        lease: &opensks_conversation::TurnSupervisorLease,
        lease_ttl_ms: u64,
        cancellation_observed: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let initial_heartbeat_ok = repo
            .heartbeat_turn_supervisor_lease(
                &lease.run_id,
                &lease.lease_owner,
                lease.fencing_token,
                lease_ttl_ms,
                timestamp_ms(),
            )
            .map_err(|error| error.to_string())?;
        if !initial_heartbeat_ok {
            return Err(format!(
                "stale turn supervisor lease `{}` for fencing token {}",
                lease.run_id, lease.fencing_token
            ));
        }

        let interval_ms = turn_lease_heartbeat_interval_ms(lease_ttl_ms);
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let workspace = workspace.to_path_buf();
        let run_id = lease.run_id.clone();
        let supervisor_id = lease.lease_owner.clone();
        let fencing_token = lease.fencing_token;
        let thread_cancellation_observed = Arc::clone(&cancellation_observed);
        let handle = std::thread::spawn(move || {
            loop {
                match stop_rx.recv_timeout(Duration::from_millis(interval_ms)) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if matches!(observe_run_cancellation(&workspace, &run_id), Ok(Some(_))) {
                            thread_cancellation_observed.store(true, Ordering::SeqCst);
                            break;
                        }
                        let Ok(repo) = opensks_conversation::ConversationRepository::open_workspace(
                            &workspace,
                        ) else {
                            continue;
                        };
                        let _ = repo.heartbeat_turn_supervisor_lease(
                            &run_id,
                            &supervisor_id,
                            fencing_token,
                            lease_ttl_ms,
                            timestamp_ms(),
                        );
                    }
                }
            }
        });

        Ok(Self {
            stop_tx: Some(stop_tx),
            handle: Some(handle),
            cancellation_observed,
            status: TurnLeaseHeartbeatStatus {
                started: true,
                initial_heartbeat_ok,
                interval_ms,
                fencing_token,
            },
        })
    }

    fn status_json(&self) -> serde_json::Value {
        serde_json::json!({
            "started": self.status.started,
            "initial_heartbeat_ok": self.status.initial_heartbeat_ok,
            "interval_ms": self.status.interval_ms,
            "fencing_token": self.status.fencing_token,
            "cancellation_observed": self.cancellation_observed.load(Ordering::SeqCst),
        })
    }
}

impl Drop for TurnLeaseHeartbeatGuard {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn turn_lease_heartbeat_interval_ms(lease_ttl_ms: u64) -> u64 {
    lease_ttl_ms.max(1).saturating_div(3).clamp(1, 10_000)
}

struct ClaimedTurnExecution<'a> {
    repo: &'a opensks_conversation::ConversationRepository,
    workspace: &'a Path,
    lease: &'a opensks_conversation::TurnSupervisorLease,
    settings: &'a opensks_contracts::ConversationTurnSettings,
    execution_workspace: &'a TurnExecutionWorkspace,
    initial_routing_decision: &'a opensks_contracts::RoutingDecision,
    explicit_local_test: bool,
    request: &'a opensks_adapter::AgentRunRequest,
    sink: &'a DaemonAgentEventSink,
    prompt: &'a str,
    agentic_config: &'a opensks_adapter::AgenticConfig,
    now_ms: u64,
}

struct ClaimedTurnSchedulerWorker<'a> {
    execution: ClaimedTurnExecution<'a>,
    root_outcome: Option<opensks_adapter::AgentRunOutcome>,
    result: Option<
        Result<
            (
                opensks_adapter::AgentRunOutcome,
                opensks_contracts::RoutingDecision,
            ),
            String,
        >,
    >,
}

impl opensks_scheduler::WorkerDriver for ClaimedTurnSchedulerWorker<'_> {
    fn acquire_holder(&mut self, _item: &opensks_contracts::SchedulerWorkItem) -> String {
        "turn-supervisor".to_string()
    }

    fn execute(
        &mut self,
        item: &opensks_contracts::SchedulerWorkItem,
    ) -> opensks_scheduler::WorkerDispatchOutcome {
        let Some(lease) = item.lease.as_ref() else {
            let message = format!(
                "scheduler work item `{}` reached worker without lease",
                item.id
            );
            if item.parent_id.is_none() {
                self.result = Some(Err(message.clone()));
            }
            return opensks_scheduler::WorkerDispatchOutcome {
                work_item_id: item.id.clone(),
                worker_id: "turn-supervisor".to_string(),
                ok: false,
                message,
                evidence_refs: vec!["scheduler:lease-missing-at-worker".to_string()],
            };
        };
        if is_objective_plan_work_item(item) {
            let root_outcome = self
                .result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map(|(outcome, _routing)| outcome)
                .or(self.root_outcome.as_ref());
            return execute_claimed_turn_objective_work_item(
                &self.execution,
                item,
                &lease.holder,
                root_outcome,
            );
        }
        if item.parent_id.is_some() {
            return execute_claimed_turn_role_work_item(&self.execution, item, &lease.holder);
        }
        let mut request = self.execution.request.clone();
        request.patch_lease = Some(opensks_adapter::PatchPathLease::from_scheduler_lease(lease));
        let result = run_claimed_turn_adapter_workload(&self.execution, &request);
        let mut evidence_refs = vec![
            "daemon:turn-scheduler-worker-route".to_string(),
            "scheduler:lease-visible-to-worker".to_string(),
            "adapter:request-patch-lease".to_string(),
        ];
        let (ok, message) = match &result {
            Ok((outcome, _)) => {
                append_apply_evidence_refs(&mut evidence_refs, outcome);
                (
                    outcome.final_state
                        == opensks_contracts::projection::RunProjectionState::Completed,
                    format!(
                        "turn supervisor worker finished with {:?}",
                        outcome.final_state
                    ),
                )
            }
            Err(error) => (false, error.clone()),
        };
        if let Ok((outcome, _routing)) = &result {
            self.root_outcome = Some(outcome.clone());
        }
        self.result = Some(result);
        opensks_scheduler::WorkerDispatchOutcome {
            work_item_id: item.id.clone(),
            worker_id: lease.holder.clone(),
            ok,
            message,
            evidence_refs,
        }
    }

    fn execute_batch(
        &mut self,
        items: Vec<opensks_contracts::SchedulerWorkItem>,
    ) -> Vec<opensks_scheduler::WorkerDispatchOutcome> {
        if items.len() <= 1
            || items.iter().any(|item| item.parent_id.is_none())
            || items.iter().any(is_objective_plan_work_item)
        {
            return items.into_iter().map(|item| self.execute(&item)).collect();
        }
        execute_claimed_turn_role_work_item_batch(&self.execution, items)
    }
}

fn execute_claimed_turn_role_work_item(
    execution: &ClaimedTurnExecution<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_id: &str,
) -> opensks_scheduler::WorkerDispatchOutcome {
    let context = RoleWorkerExecutionContext::from_claimed_turn(execution);
    execute_claimed_turn_role_work_item_with_context(&context, item, worker_id, None)
}

fn is_objective_plan_work_item(item: &opensks_contracts::SchedulerWorkItem) -> bool {
    item.evidence_refs
        .iter()
        .any(|evidence| evidence == "scheduler:objective-plan-work-item")
}

fn execute_claimed_turn_objective_work_item(
    execution: &ClaimedTurnExecution<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_id: &str,
    root_outcome: Option<&opensks_adapter::AgentRunOutcome>,
) -> opensks_scheduler::WorkerDispatchOutcome {
    match item.node_id.as_str() {
        "apply" => {
            return execute_claimed_turn_objective_apply_work_item(
                execution,
                item,
                worker_id,
                root_outcome,
            );
        }
        "seal" => {
            return execute_claimed_turn_objective_seal_work_item(execution, item, worker_id);
        }
        _ => {}
    }

    if let Some(role) = objective_plan_role_for_item(item) {
        let mut role_item = item.clone();
        role_item.requirement_ids = vec![execution.lease.turn_id.clone(), role.to_string()];
        push_unique_string(
            &mut role_item.evidence_refs,
            "daemon:objective-plan-child-runtime".to_string(),
        );
        let mut outcome = execute_claimed_turn_role_work_item(execution, &role_item, worker_id);
        push_unique_string(
            &mut outcome.evidence_refs,
            "daemon:objective-plan-child-executed".to_string(),
        );
        push_unique_string(
            &mut outcome.evidence_refs,
            "scheduler:objective-plan-work-item".to_string(),
        );
        outcome.message = format!("objective planner {role} child: {}", outcome.message);
        return outcome;
    }

    if is_claimed_turn_objective_runtime_dispatchable(item) {
        let mut evidence_refs = item.evidence_refs.clone();
        push_unique_string(
            &mut evidence_refs,
            "daemon:objective-plan-child-executed".to_string(),
        );
        push_unique_string(
            &mut evidence_refs,
            "daemon:objective-plan-control-node".to_string(),
        );
        return opensks_scheduler::WorkerDispatchOutcome {
            work_item_id: item.id.clone(),
            worker_id: worker_id.to_string(),
            ok: true,
            message: format!(
                "objective planner control node `{}` completed without external side effects",
                item.node_id
            ),
            evidence_refs,
        };
    }

    opensks_scheduler::WorkerDispatchOutcome {
        work_item_id: item.id.clone(),
        worker_id: worker_id.to_string(),
        ok: false,
        message: format!(
            "objective planner node `{}` is not yet supported by claimed-turn runtime",
            item.node_id
        ),
        evidence_refs: vec![
            "daemon:objective-plan-child-unsupported".to_string(),
            "scheduler:objective-plan-work-item".to_string(),
        ],
    }
}

fn execute_claimed_turn_objective_apply_work_item(
    execution: &ClaimedTurnExecution<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_id: &str,
    root_outcome: Option<&opensks_adapter::AgentRunOutcome>,
) -> opensks_scheduler::WorkerDispatchOutcome {
    let approval_id = format!("approval-integration-{}", execution.lease.run_id);
    let mut evidence_refs = item.evidence_refs.clone();
    push_unique_string(
        &mut evidence_refs,
        "daemon:objective-plan-child-executed".to_string(),
    );
    push_unique_string(
        &mut evidence_refs,
        "daemon:objective-plan-apply-runtime".to_string(),
    );
    if let Err(error) = ensure_objective_integration_candidate(execution, root_outcome) {
        push_unique_string(
            &mut evidence_refs,
            "daemon:objective-plan-apply-candidate-unavailable".to_string(),
        );
        return opensks_scheduler::WorkerDispatchOutcome {
            work_item_id: item.id.clone(),
            worker_id: worker_id.to_string(),
            ok: false,
            message: format!("objective planner apply child candidate unavailable: {error}"),
            evidence_refs,
        };
    }
    let receipt = match apply_integration_candidate(
        execution.workspace,
        &execution.lease.run_id,
        Some(&approval_id),
        execution.now_ms,
    ) {
        Ok(receipt) => receipt,
        Err(error) => {
            push_unique_string(
                &mut evidence_refs,
                "daemon:objective-plan-apply-failed".to_string(),
            );
            return opensks_scheduler::WorkerDispatchOutcome {
                work_item_id: item.id.clone(),
                worker_id: worker_id.to_string(),
                ok: false,
                message: format!("objective planner apply child failed: {error}"),
                evidence_refs,
            };
        }
    };
    if (receipt.state != "failed" || receipt.main_workspace_modified)
        && let Err(error) = append_integration_apply_event(execution.workspace, &receipt)
    {
        push_unique_string(
            &mut evidence_refs,
            "daemon:objective-plan-apply-event-append-failed".to_string(),
        );
        return opensks_scheduler::WorkerDispatchOutcome {
            work_item_id: item.id.clone(),
            worker_id: worker_id.to_string(),
            ok: false,
            message: format!("objective planner apply child event append failed: {error}"),
            evidence_refs,
        };
    }
    if receipt.verification_ref.is_some() {
        push_unique_string(
            &mut evidence_refs,
            "integration:verification-receipt".to_string(),
        );
    }
    let ok = match receipt.state.as_str() {
        "integrated" => {
            push_unique_string(
                &mut evidence_refs,
                "integration:approval-gated-apply".to_string(),
            );
            push_unique_string(&mut evidence_refs, "integration:final-seal".to_string());
            true
        }
        "awaiting_approval" => {
            push_unique_string(
                &mut evidence_refs,
                "daemon:objective-plan-apply-awaiting-approval".to_string(),
            );
            true
        }
        _ if !receipt.main_workspace_modified => {
            push_unique_string(
                &mut evidence_refs,
                "daemon:objective-plan-apply-blocked-before-mutation".to_string(),
            );
            true
        }
        _ => {
            push_unique_string(
                &mut evidence_refs,
                "daemon:objective-plan-apply-failed".to_string(),
            );
            false
        }
    };
    opensks_scheduler::WorkerDispatchOutcome {
        work_item_id: item.id.clone(),
        worker_id: worker_id.to_string(),
        ok,
        message: format!(
            "objective planner apply child {} with reason {}",
            receipt.state, receipt.reason_code
        ),
        evidence_refs,
    }
}

fn ensure_objective_integration_candidate(
    execution: &ClaimedTurnExecution<'_>,
    root_outcome: Option<&opensks_adapter::AgentRunOutcome>,
) -> Result<(), String> {
    let candidate_path = integration_candidate_artifact_path(
        execution.workspace,
        &execution.lease.run_id,
        "candidate.json",
    );
    if candidate_path.exists() {
        return Ok(());
    }
    let Some(root_outcome) = root_outcome else {
        return Err("root outcome unavailable before objective apply".to_string());
    };
    let Some(_candidate) = write_integration_candidate(
        execution.workspace,
        execution.lease,
        execution.settings,
        execution.execution_workspace,
        root_outcome,
        execution.now_ms,
    )?
    else {
        return Err("no integration candidate sources available".to_string());
    };
    Ok(())
}

fn execute_claimed_turn_objective_seal_work_item(
    execution: &ClaimedTurnExecution<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_id: &str,
) -> opensks_scheduler::WorkerDispatchOutcome {
    let mut evidence_refs = item.evidence_refs.clone();
    push_unique_string(
        &mut evidence_refs,
        "daemon:objective-plan-child-executed".to_string(),
    );
    push_unique_string(
        &mut evidence_refs,
        "daemon:objective-plan-seal-runtime".to_string(),
    );
    let seal_path = integration_candidate_artifact_path(
        execution.workspace,
        &execution.lease.run_id,
        "seal.json",
    );
    match std::fs::read_to_string(&seal_path) {
        Ok(raw) => match serde_json::from_str::<opensks_contracts::IntegrationFinalSeal>(&raw) {
            Ok(seal) if seal.state == "sealed" => {
                push_unique_string(&mut evidence_refs, "integration:final-seal".to_string());
                opensks_scheduler::WorkerDispatchOutcome {
                    work_item_id: item.id.clone(),
                    worker_id: worker_id.to_string(),
                    ok: true,
                    message: format!(
                        "objective planner seal child observed final seal {}",
                        seal.seal_ref
                    ),
                    evidence_refs,
                }
            }
            Ok(seal) => {
                push_unique_string(
                    &mut evidence_refs,
                    "daemon:objective-plan-seal-invalid".to_string(),
                );
                opensks_scheduler::WorkerDispatchOutcome {
                    work_item_id: item.id.clone(),
                    worker_id: worker_id.to_string(),
                    ok: false,
                    message: format!(
                        "objective planner seal child found non-sealed state {}",
                        seal.state
                    ),
                    evidence_refs,
                }
            }
            Err(error) => {
                push_unique_string(
                    &mut evidence_refs,
                    "daemon:objective-plan-seal-invalid".to_string(),
                );
                opensks_scheduler::WorkerDispatchOutcome {
                    work_item_id: item.id.clone(),
                    worker_id: worker_id.to_string(),
                    ok: false,
                    message: format!("objective planner seal child could not decode seal: {error}"),
                    evidence_refs,
                }
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            push_unique_string(
                &mut evidence_refs,
                "daemon:objective-plan-seal-pending-apply".to_string(),
            );
            opensks_scheduler::WorkerDispatchOutcome {
                work_item_id: item.id.clone(),
                worker_id: worker_id.to_string(),
                ok: true,
                message: "objective planner seal child pending approval-gated integration apply"
                    .to_string(),
                evidence_refs,
            }
        }
        Err(error) => {
            push_unique_string(
                &mut evidence_refs,
                "daemon:objective-plan-seal-read-failed".to_string(),
            );
            opensks_scheduler::WorkerDispatchOutcome {
                work_item_id: item.id.clone(),
                worker_id: worker_id.to_string(),
                ok: false,
                message: format!("objective planner seal child could not read seal: {error}"),
                evidence_refs,
            }
        }
    }
}

fn integration_candidate_artifact_path(workspace: &Path, run_id: &str, file_name: &str) -> PathBuf {
    workspace
        .join(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join(file_name)
}

fn objective_plan_role_for_item(
    item: &opensks_contracts::SchedulerWorkItem,
) -> Option<&'static str> {
    match item.node_id.as_str() {
        "workers" => Some("code"),
        "verifier" => Some("verification"),
        _ => None,
    }
}

fn execute_claimed_turn_role_work_item_batch(
    execution: &ClaimedTurnExecution<'_>,
    items: Vec<opensks_contracts::SchedulerWorkItem>,
) -> Vec<opensks_scheduler::WorkerDispatchOutcome> {
    let batch_size = items.len();
    let context = RoleWorkerExecutionContext::from_claimed_turn(execution);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(batch_size);
        for (lane_index, item) in items.into_iter().enumerate() {
            let item_id = item.id.clone();
            let context = &context;
            handles.push((
                item_id,
                scope.spawn(move || {
                    let Some(lease) = item.lease.as_ref() else {
                        return opensks_scheduler::WorkerDispatchOutcome {
                            work_item_id: item.id.clone(),
                            worker_id: "turn-supervisor".to_string(),
                            ok: false,
                            message: format!(
                                "scheduler work item `{}` reached role batch without lease",
                                item.id
                            ),
                            evidence_refs: vec![
                                "scheduler:lease-missing-at-worker".to_string(),
                                "daemon:role-worker-parallel-batch".to_string(),
                            ],
                        };
                    };
                    execute_claimed_turn_role_work_item_with_context(
                        context,
                        &item,
                        &lease.holder,
                        Some(RoleWorkerBatchContext {
                            batch_size,
                            lane_index,
                        }),
                    )
                }),
            ));
        }
        handles
            .into_iter()
            .map(|(item_id, handle)| match handle.join() {
                Ok(outcome) => outcome,
                Err(_panic) => opensks_scheduler::WorkerDispatchOutcome {
                    work_item_id: item_id,
                    worker_id: "turn-supervisor".to_string(),
                    ok: false,
                    message: "role worker thread panicked".to_string(),
                    evidence_refs: vec![
                        "daemon:role-worker-parallel-batch".to_string(),
                        "daemon:role-worker-thread-panic".to_string(),
                    ],
                },
            })
            .collect()
    })
}

struct RoleWorkerExecutionContext<'a> {
    workspace: &'a Path,
    turn_id: &'a str,
    prompt: &'a str,
    request: &'a opensks_adapter::AgentRunRequest,
    sink: &'a DaemonAgentEventSink,
    agentic_config: &'a opensks_adapter::AgenticConfig,
}

impl<'a> RoleWorkerExecutionContext<'a> {
    fn from_claimed_turn(execution: &'a ClaimedTurnExecution<'a>) -> Self {
        Self {
            workspace: execution.workspace,
            turn_id: &execution.lease.turn_id,
            prompt: execution.prompt,
            request: execution.request,
            sink: execution.sink,
            agentic_config: execution.agentic_config,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RoleWorkerBatchContext {
    batch_size: usize,
    lane_index: usize,
}

#[derive(Debug, Clone)]
struct WorkerContextPackMaterialization {
    artifact_ref: String,
    body: String,
}

fn materialize_role_worker_context_pack(
    context: &RoleWorkerExecutionContext<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    role: &str,
) -> Result<Option<WorkerContextPackMaterialization>, String> {
    let Some(root_ref) = item.context_pack_ref.as_deref() else {
        return Ok(None);
    };
    let Some(worker_ref) = item.worker_context_pack_ref.as_deref() else {
        return Ok(None);
    };
    let root_path = context_artifact_path(context.workspace, root_ref)
        .ok_or_else(|| "worker_context_root_ref_invalid".to_string())?;
    let worker_path = context_artifact_path(context.workspace, worker_ref)
        .ok_or_else(|| "worker_context_ref_invalid".to_string())?;
    let root_json = std::fs::read_to_string(&root_path)
        .map_err(|_error| "worker_context_root_read_failed".to_string())?;
    let root_pack: opensks_contracts::ContextPack = serde_json::from_str(&root_json)
        .map_err(|_error| "worker_context_root_decode_failed".to_string())?;
    let pack_id = worker_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| "worker_context_pack_id_invalid".to_string())?
        .to_string();
    let token_budget = root_pack.token_budget.clamp(128, 768);
    let pack = opensks_context::build_worker_context_pack(
        &root_pack,
        pack_id,
        &item.id,
        role,
        &item.node_id,
        token_budget,
    );
    let written = opensks_context::write_context_pack(context.workspace, &pack)
        .map_err(|_error| "worker_context_write_failed".to_string())?;
    if written != worker_path {
        return Err("worker_context_write_path_mismatch".to_string());
    }
    Ok(Some(WorkerContextPackMaterialization {
        artifact_ref: worker_ref.to_string(),
        body: pack.body,
    }))
}

fn context_artifact_path(workspace: &Path, reference: &str) -> Option<PathBuf> {
    let base_ref = reference.split('#').next().unwrap_or(reference);
    let relative = base_ref.strip_prefix("artifact://")?;
    let relative_path = Path::new(relative);
    if relative_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(workspace.join(relative_path))
}

fn execute_claimed_turn_role_work_item_with_context(
    context: &RoleWorkerExecutionContext<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_id: &str,
    batch: Option<RoleWorkerBatchContext>,
) -> opensks_scheduler::WorkerDispatchOutcome {
    let role = item
        .requirement_ids
        .iter()
        .find(|id| id.as_str() != context.turn_id)
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    let sequence_base = role_worker_sequence_base(item);
    let worker_context = match materialize_role_worker_context_pack(context, item, &role) {
        Ok(worker_context) => worker_context,
        Err(_error) => {
            opensks_adapter::AgentEventSink::emit(
                context.sink,
                role_worker_agent_event(
                    context.request,
                    item,
                    worker_id,
                    sequence_base,
                    opensks_contracts::AgentEventKind::Error,
                    serde_json::json!({
                        "code": "role_worker_context_pack_failed",
                        "work_item_id": item.id,
                        "role": role,
                        "provider_id": item.provider_selector,
                        "model_id": item.model_selector,
                        "parent_work_item_id": item.parent_id,
                        "context_pack_ref": item.context_pack_ref,
                        "worker_context_pack_ref": item.worker_context_pack_ref,
                        "diagnostic_redacted": true,
                        "path_redacted": true,
                        "content_redacted": true
                    }),
                ),
            );
            return opensks_scheduler::WorkerDispatchOutcome {
                work_item_id: item.id.clone(),
                worker_id: worker_id.to_string(),
                ok: false,
                message: format!("role worker `{role}` could not materialize worker context pack"),
                evidence_refs: vec![
                    "daemon:role-worker-context-pack-failed".to_string(),
                    "context:worker-context-pack".to_string(),
                ],
            };
        }
    };
    let worker_context_pack_ref = worker_context
        .as_ref()
        .map(|context| context.artifact_ref.as_str())
        .or(item.worker_context_pack_ref.as_deref());
    opensks_adapter::AgentEventSink::emit(
        context.sink,
        role_worker_agent_event(
            context.request,
            item,
            worker_id,
            sequence_base,
            opensks_contracts::AgentEventKind::WorkerSpawned,
            serde_json::json!({
                "code": "role_worker_started",
                "work_item_id": item.id,
                "role": role,
                "provider_id": item.provider_selector,
                "model_id": item.model_selector,
                "parent_work_item_id": item.parent_id,
                "capability_requirements": item.capability_requirements,
                "model_call": true,
                "parallel_batch": batch.map(|batch| batch.batch_size > 1).unwrap_or(false),
                "parallel_batch_size": batch.map(|batch| batch.batch_size),
                "parallel_lane_index": batch.map(|batch| batch.lane_index),
                "context_pack_ref": item.context_pack_ref,
                "worker_context_pack_ref": worker_context_pack_ref,
                "worker_context_pack_materialized": worker_context.is_some(),
                "path_redacted": true,
                "content_redacted": true
            }),
        ),
    );
    let role_call = execute_claimed_turn_role_model_call(
        context,
        item,
        worker_id,
        &role,
        worker_context.as_ref(),
    );
    let (
        ok,
        reason_code,
        response_hash,
        response_bytes,
        code_candidate_ref,
        code_candidate_patch_ref,
        code_candidate_target_count,
        semantic_verifier_judgment_ref,
        semantic_verifier_judgment_state,
        evidence_refs,
    ) = match role_call {
        Ok(receipt) => {
            let mut evidence_refs = vec![
                "daemon:role-worker-executed".to_string(),
                "daemon:role-worker-model-call".to_string(),
                "scheduler:role-plan-work-item".to_string(),
                "provider:role-routing".to_string(),
            ];
            if batch.is_some_and(|batch| batch.batch_size > 1) {
                evidence_refs.push("daemon:role-worker-parallel-batch".to_string());
            }
            if receipt.code_candidate.is_some() {
                evidence_refs.push("daemon:role-worker-code-candidate".to_string());
            }
            if receipt.semantic_verifier_judgment.is_some() {
                evidence_refs.push("daemon:semantic-verifier-judgment".to_string());
            }
            if worker_context.is_some() {
                evidence_refs.push("context:worker-context-pack".to_string());
                evidence_refs.push("opensks-context:worker-scoped-context-pack".to_string());
            }
            let code_candidate_ref = receipt
                .code_candidate
                .as_ref()
                .map(|candidate| candidate.receipt_ref.clone());
            let code_candidate_patch_ref = receipt
                .code_candidate
                .as_ref()
                .map(|candidate| candidate.patch_ref.clone());
            let code_candidate_target_count = receipt
                .code_candidate
                .as_ref()
                .map(|candidate| candidate.target_paths.len())
                .unwrap_or(0);
            let semantic_verifier_judgment_ref = receipt
                .semantic_verifier_judgment
                .as_ref()
                .map(|judgment| judgment.judgment_ref.clone());
            let semantic_verifier_judgment_state = receipt
                .semantic_verifier_judgment
                .as_ref()
                .map(|judgment| judgment.state.clone());
            (
                true,
                "role_worker_model_call_completed",
                Some(receipt.response_hash),
                Some(receipt.response_bytes),
                code_candidate_ref,
                code_candidate_patch_ref,
                code_candidate_target_count,
                semantic_verifier_judgment_ref,
                semantic_verifier_judgment_state,
                evidence_refs,
            )
        }
        Err(_error) => {
            opensks_adapter::AgentEventSink::emit(
                context.sink,
                role_worker_agent_event(
                    context.request,
                    item,
                    worker_id,
                    sequence_base + 1,
                    opensks_contracts::AgentEventKind::Error,
                    serde_json::json!({
                        "code": "role_worker_model_call_failed",
                        "work_item_id": item.id,
                        "role": role,
                        "provider_id": item.provider_selector,
                        "model_id": item.model_selector,
                        "parent_work_item_id": item.parent_id,
                        "reason_code": "provider_role_call_failed",
                        "parallel_batch": batch.map(|batch| batch.batch_size > 1).unwrap_or(false),
                        "parallel_batch_size": batch.map(|batch| batch.batch_size),
                        "parallel_lane_index": batch.map(|batch| batch.lane_index),
                        "context_pack_ref": item.context_pack_ref,
                        "worker_context_pack_ref": worker_context_pack_ref,
                        "worker_context_pack_materialized": worker_context.is_some(),
                        "diagnostic_redacted": true,
                        "path_redacted": true,
                        "content_redacted": true
                    }),
                ),
            );
            let mut evidence_refs = vec![
                "daemon:role-worker-model-call-failed".to_string(),
                "scheduler:role-plan-work-item".to_string(),
                "provider:role-routing".to_string(),
            ];
            if batch.is_some_and(|batch| batch.batch_size > 1) {
                evidence_refs.push("daemon:role-worker-parallel-batch".to_string());
            }
            if worker_context.is_some() {
                evidence_refs.push("context:worker-context-pack".to_string());
                evidence_refs.push("opensks-context:worker-scoped-context-pack".to_string());
            }
            return opensks_scheduler::WorkerDispatchOutcome {
                work_item_id: item.id.clone(),
                worker_id: worker_id.to_string(),
                ok: false,
                message: format!(
                    "role worker `{}` failed provider model call for {}",
                    role, item.id
                ),
                evidence_refs,
            };
        }
    };
    opensks_adapter::AgentEventSink::emit(
        context.sink,
        role_worker_agent_event(
            context.request,
            item,
            worker_id,
            sequence_base + 1,
            opensks_contracts::AgentEventKind::WorkerCompleted,
            serde_json::json!({
                "code": "role_worker_completed",
                "work_item_id": item.id,
                "role": role,
                "provider_id": item.provider_selector,
                "model_id": item.model_selector,
                "parent_work_item_id": item.parent_id,
                "reason_code": reason_code,
                "model_call": true,
                "response_hash": response_hash,
                "response_bytes": response_bytes,
                "code_candidate_ref": code_candidate_ref,
                "code_candidate_patch_ref": code_candidate_patch_ref,
                "code_candidate_target_count": code_candidate_target_count,
                "semantic_verifier_judgment_ref": semantic_verifier_judgment_ref,
                "semantic_verifier_judgment_state": semantic_verifier_judgment_state,
                "parallel_batch": batch.map(|batch| batch.batch_size > 1).unwrap_or(false),
                "parallel_batch_size": batch.map(|batch| batch.batch_size),
                "parallel_lane_index": batch.map(|batch| batch.lane_index),
                "context_pack_ref": item.context_pack_ref,
                "worker_context_pack_ref": worker_context_pack_ref,
                "worker_context_pack_materialized": worker_context.is_some(),
                "path_redacted": true,
                "content_redacted": true
            }),
        ),
    );
    opensks_scheduler::WorkerDispatchOutcome {
        work_item_id: item.id.clone(),
        worker_id: worker_id.to_string(),
        ok,
        message: format!(
            "role worker `{}` completed provider model call for {}",
            role, item.id
        ),
        evidence_refs,
    }
}

#[derive(Debug, Clone)]
struct RoleWorkerModelReceipt {
    response_hash: String,
    response_bytes: usize,
    code_candidate: Option<RoleSubcontractCandidateReceipt>,
    semantic_verifier_judgment: Option<SemanticVerifierJudgmentReceipt>,
}

fn execute_claimed_turn_role_model_call(
    context: &RoleWorkerExecutionContext<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_id: &str,
    role: &str,
    worker_context: Option<&WorkerContextPackMaterialization>,
) -> Result<RoleWorkerModelReceipt, String> {
    if role == "code" {
        return execute_claimed_turn_code_role_subcontract(
            context,
            item,
            worker_id,
            role,
            worker_context,
        );
    }
    let dispatch = prepare_role_provider_dispatch(context.workspace, item)?;
    let completer = opensks_adapter::OpenAiCompatibleChatCompleter::new(
        dispatch.connection.endpoint.base_url.clone(),
        dispatch.bearer_token.clone(),
    )
    .map_err(|_error| "provider_role_call_failed".to_string())?;
    let response = opensks_adapter::ChatCompleter::complete(
        &completer,
        &role_worker_model_call_body(
            &dispatch.model.remote_model_id,
            max_output_tokens_for_model(&dispatch.model).min(256),
            role,
            context,
            item,
            worker_context,
            provider_reasoning_effort_for_provider(
                dispatch.connection.kind,
                context.agentic_config.reasoning_effort,
            ),
        ),
    )
    .map_err(|_error| "provider_role_call_failed".to_string())?;
    let content = response
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "provider_role_call_returned_no_text".to_string())?;
    let response_hash = stable_text_digest(content);
    let response_bytes = content.len();
    let semantic_verifier_judgment = if role == "verification" {
        Some(write_semantic_verifier_judgment(
            context.workspace,
            item,
            worker_id,
            role,
            &dispatch,
            content,
            &response_hash,
            response_bytes,
            worker_context,
            context.request.now_ms,
        )?)
    } else {
        None
    };
    Ok(RoleWorkerModelReceipt {
        response_hash,
        response_bytes,
        code_candidate: None,
        semantic_verifier_judgment,
    })
}

fn execute_claimed_turn_code_role_subcontract(
    context: &RoleWorkerExecutionContext<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    _worker_id: &str,
    role: &str,
    worker_context: Option<&WorkerContextPackMaterialization>,
) -> Result<RoleWorkerModelReceipt, String> {
    let dispatch = prepare_role_provider_dispatch(context.workspace, item)?;
    let isolation_worker_id = role_subcontract_worker_id(item);
    let isolation = opensks_git::create_isolation(
        context.workspace,
        &context.request.run_id,
        &isolation_worker_id,
    )
    .map_err(|error| format!("role code isolation failed: {error}"))?;
    let mut request = context.request.clone();
    request.workspace = PathBuf::from(&isolation.worktree_path);
    request.patch_lease = item
        .lease
        .as_ref()
        .map(opensks_adapter::PatchPathLease::from_scheduler_lease);
    let completer = opensks_adapter::OpenAiCompatibleChatCompleter::new(
        dispatch.connection.endpoint.base_url,
        dispatch.bearer_token,
    )
    .map_err(|_error| "provider_role_call_failed".to_string())?;
    let mut driver = opensks_adapter::OpenRouterToolDriver::new(
        dispatch.model.remote_model_id.clone(),
        max_output_tokens_for_model(&dispatch.model),
        completer,
        "You are an isolated OpenSKS Code role subcontract worker. Use workspace tools only inside this isolated workspace. Produce a small candidate patch when useful, never claim it is integrated into the main workspace, and keep final text concise.",
        format!(
            "Role: {role}\nWork item: {}\nRoot context pack ref: {}\nWorker context pack ref: {}\nWorker context:\n{}\n\nUser objective:\n{}\n\nIf a safe code/documentation patch is useful, create it in this isolated workspace. Final answer should summarize the role candidate.",
            item.id,
            item.context_pack_ref.as_deref().unwrap_or("none"),
            worker_context_ref(item, worker_context),
            worker_context_body(worker_context),
            context.prompt
        ),
    )
    .with_chat_reasoning_effort_if_some(
        chat_reasoning_effort_wire_for_provider(dispatch.connection.kind),
        context.agentic_config.reasoning_effort,
    )
    .with_tools(opensks_adapter::tool_definitions());
    let role_sink = RoleSubcontractAgentEventSink {
        inner: context.sink,
        worker_id: isolation_worker_id.clone(),
        node_id: Some(item.node_id.clone()),
        sequence_base: role_worker_sequence_base(item).saturating_add(100),
    };
    let outcome = opensks_adapter::run_agentic_loop(
        &request,
        &mut driver,
        context.agentic_config,
        &role_sink,
    )
    .map_err(|error| format!("role code agentic loop failed: {error}"))?;
    let code_candidate = write_role_subcontract_candidate(
        context.workspace,
        item,
        role,
        &isolation,
        &outcome,
        context.request.now_ms,
    )?;
    Ok(RoleWorkerModelReceipt {
        response_hash: stable_text_digest(&outcome.assistant_text),
        response_bytes: outcome.assistant_text.len(),
        code_candidate,
        semantic_verifier_judgment: None,
    })
}

type RoleSubcontractCandidateReceipt = opensks_contracts::RoleSubcontractCandidateReceipt;
type SemanticVerifierJudgmentReceipt = opensks_contracts::SemanticVerifierJudgmentReceipt;

#[allow(clippy::too_many_arguments)]
fn write_semantic_verifier_judgment(
    workspace: &Path,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_id: &str,
    role: &str,
    dispatch: &RoleProviderDispatchTarget,
    response_text: &str,
    response_hash: &str,
    response_bytes: usize,
    worker_context: Option<&WorkerContextPackMaterialization>,
    now_ms: u64,
) -> Result<SemanticVerifierJudgmentReceipt, String> {
    let safe_item_id = safe_artifact_segment(&item.id);
    let relative_dir = PathBuf::from(".opensks")
        .join("runtime")
        .join("semantic-verifiers")
        .join(&item.run_id)
        .join(&safe_item_id);
    let dir = workspace.join(&relative_dir);
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let judgment_relative = relative_dir.join("judgment.json");
    let judgment_ref = artifact_ref(&judgment_relative);
    let semantic_verdict = parse_semantic_verifier_verdict(response_text);
    let receipt = SemanticVerifierJudgmentReceipt {
        schema: opensks_contracts::SEMANTIC_VERIFIER_JUDGMENT_SCHEMA.to_string(),
        id: format!("semantic-verifier-{}-{safe_item_id}", item.run_id),
        run_id: item.run_id.clone(),
        work_item_id: item.id.clone(),
        role: role.to_string(),
        worker_id: worker_id.to_string(),
        state: "judgment_ready".to_string(),
        reason_code: "model_semantic_verifier_judgment_recorded".to_string(),
        verifier_kind: "model_semantic_judgment".to_string(),
        verdict: semantic_verdict.verdict.to_string(),
        passed_gates: semantic_verdict.passed_gates,
        failed_gates: semantic_verdict.failed_gates,
        provider_id: item
            .provider_selector
            .clone()
            .or_else(|| Some(dispatch.connection.id.clone())),
        model_id: item
            .model_selector
            .clone()
            .or_else(|| Some(dispatch.model.id.clone())),
        response_hash: response_hash.to_string(),
        response_bytes,
        judgment_ref: judgment_ref.clone(),
        context_pack_ref: item.context_pack_ref.clone(),
        worker_context_pack_ref: worker_context
            .map(|context| context.artifact_ref.clone())
            .or_else(|| item.worker_context_pack_ref.clone()),
        path_redacted: true,
        content_redacted: true,
        generated_at_ms: now_ms,
        evidence_refs: vec![
            "daemon:semantic-verifier-judgment".to_string(),
            "daemon:role-worker-model-call".to_string(),
            "provider:role-routing".to_string(),
            "scheduler:role-plan-work-item".to_string(),
        ],
    };
    std::fs::write(
        dir.join("judgment.json"),
        serde_json::to_string_pretty(&receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(receipt)
}

struct SemanticVerifierParsedVerdict {
    verdict: &'static str,
    passed_gates: Vec<String>,
    failed_gates: Vec<String>,
}

fn parse_semantic_verifier_verdict(response_text: &str) -> SemanticVerifierParsedVerdict {
    match explicit_semantic_verifier_verdict(response_text) {
        Some("pass") => SemanticVerifierParsedVerdict {
            verdict: "pass",
            passed_gates: vec!["semantic_verifier_verdict_passed".to_string()],
            failed_gates: Vec::new(),
        },
        Some("fail") => SemanticVerifierParsedVerdict {
            verdict: "fail",
            passed_gates: Vec::new(),
            failed_gates: vec!["semantic_verifier_verdict_passed".to_string()],
        },
        _ => SemanticVerifierParsedVerdict {
            verdict: "unknown",
            passed_gates: Vec::new(),
            failed_gates: vec!["semantic_verifier_verdict_present".to_string()],
        },
    }
}

fn explicit_semantic_verifier_verdict(response_text: &str) -> Option<&'static str> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(response_text) {
        if let Some(verdict) = value.get("verdict").and_then(serde_json::Value::as_str) {
            return normalized_semantic_verdict(verdict);
        }
        if let Some(passed) = value.get("passed").and_then(serde_json::Value::as_bool) {
            return Some(if passed { "pass" } else { "fail" });
        }
    }
    for line in response_text.lines().take(12) {
        let normalized = line.trim().trim_matches(['{', '}', '"', ',', ' ']);
        let lower = normalized.to_ascii_lowercase();
        if lower.starts_with("verdict") || lower.starts_with("\"verdict\"") {
            return normalized_semantic_verdict(&lower);
        }
    }
    None
}

fn normalized_semantic_verdict(value: &str) -> Option<&'static str> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("fail") || lower.contains("reject") || lower.contains("block") {
        Some("fail")
    } else if lower.contains("pass") || lower.contains("approve") {
        Some("pass")
    } else {
        None
    }
}

fn write_role_subcontract_candidate(
    workspace: &Path,
    item: &opensks_contracts::SchedulerWorkItem,
    role: &str,
    isolation: &opensks_contracts::GitIsolationReport,
    outcome: &opensks_adapter::AgentRunOutcome,
    now_ms: u64,
) -> Result<Option<RoleSubcontractCandidateReceipt>, String> {
    if !outcome.apply_results.iter().any(|result| result.applied) {
        return Ok(None);
    }
    let mut target_paths = BTreeSet::new();
    for patch in &outcome.patches {
        for file in &patch.files {
            target_paths.insert(file.path.clone());
        }
    }
    let target_paths = target_paths.into_iter().collect::<Vec<_>>();
    if target_paths.is_empty() {
        return Ok(None);
    }
    let safe_item_id = safe_artifact_segment(&item.id);
    let relative_dir = PathBuf::from(".opensks")
        .join("runtime")
        .join("role-candidates")
        .join(&item.run_id)
        .join(&safe_item_id);
    let dir = workspace.join(&relative_dir);
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let patch_relative = relative_dir.join("candidate.patch");
    let receipt_relative = relative_dir.join("candidate.json");
    let patch_ref = artifact_ref(&patch_relative);
    let receipt_ref = artifact_ref(&receipt_relative);
    let unified_diff = outcome
        .patches
        .iter()
        .flat_map(|patch| patch.files.iter())
        .map(|file| file.unified_diff.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.join("candidate.patch"), unified_diff).map_err(|error| error.to_string())?;
    let applied_files = outcome
        .apply_results
        .iter()
        .flat_map(|result| result.applied_files.iter().cloned())
        .collect::<Vec<_>>();
    let (
        shard_policy_selection_policy,
        planner_required_source_candidate_count,
        planner_required_verifier_count,
    ) = if item.shard_policy_id.is_some() {
        (
            Some(
                item.shard_policy_selection_policy
                    .clone()
                    .ok_or_else(|| "planner_shard_policy_selection_policy_missing".to_string())?,
            ),
            item.shard_policy_required_source_count
                .filter(|count| *count > 0)
                .ok_or_else(|| "planner_shard_policy_required_source_count_missing".to_string())?,
            item.shard_policy_required_verifier_count.ok_or_else(|| {
                "planner_shard_policy_required_verifier_count_missing".to_string()
            })?,
        )
    } else {
        (None, 0, 0)
    };
    let receipt = RoleSubcontractCandidateReceipt {
        schema: opensks_contracts::ROLE_SUBCONTRACT_CANDIDATE_RECEIPT_SCHEMA.to_string(),
        id: format!("role-candidate-{}-{safe_item_id}", item.run_id),
        run_id: item.run_id.clone(),
        work_item_id: item.id.clone(),
        role: role.to_string(),
        worker_id: role_subcontract_worker_id(item),
        state: "candidate_ready".to_string(),
        reason_code: "isolated_role_patch_candidate_ready".to_string(),
        source_isolation_id: isolation.id.clone(),
        source_isolation_mode: isolation_mode_label(&isolation.mode).to_string(),
        source_base_commit: isolation.base_commit.clone(),
        source_git_available: isolation.git_available,
        target_paths,
        patch_count: outcome.patches.len(),
        apply_result_count: outcome.apply_results.len(),
        applied_files,
        shard_policy_id: item.shard_policy_id.clone(),
        shard_policy_selection_policy,
        planner_required_source_candidate_count,
        planner_required_verifier_count,
        receipt_ref,
        patch_ref,
        main_workspace_modified: false,
        integration_required: true,
        approval_required: true,
        path_redacted: true,
        content_redacted: true,
        generated_at_ms: now_ms,
        evidence_refs: vec![
            "git:role-isolation-prepared".to_string(),
            "patch-engine:atomic-apply".to_string(),
            "daemon:role-worker-code-candidate".to_string(),
        ],
    };
    std::fs::write(
        dir.join("candidate.json"),
        serde_json::to_string_pretty(&receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(Some(receipt))
}

struct RoleSubcontractAgentEventSink<'a> {
    inner: &'a DaemonAgentEventSink,
    worker_id: String,
    node_id: Option<String>,
    sequence_base: u64,
}

impl opensks_adapter::AgentEventSink for RoleSubcontractAgentEventSink<'_> {
    fn emit(&self, mut event: opensks_contracts::AgentEventEnvelope) {
        event.worker_id = Some(self.worker_id.clone());
        event.node_id = self.node_id.clone();
        event.sequence = self.sequence_base.saturating_add(event.sequence);
        self.inner.emit(event);
    }
}

fn role_subcontract_worker_id(item: &opensks_contracts::SchedulerWorkItem) -> String {
    format!("role-subcontract-{}", safe_artifact_segment(&item.id))
}

fn safe_artifact_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unnamed".to_string()
    } else {
        sanitized
    }
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[derive(Debug, Clone)]
struct RoleProviderDispatchTarget {
    connection: opensks_contracts::ProviderConnection,
    model: opensks_contracts::ModelCatalogEntry,
    bearer_token: String,
}

fn prepare_role_provider_dispatch(
    workspace: &Path,
    item: &opensks_contracts::SchedulerWorkItem,
) -> Result<RoleProviderDispatchTarget, String> {
    let model_id = item
        .model_selector
        .as_deref()
        .ok_or_else(|| "role work item has no selected model".to_string())?;
    let provider_repo = opensks_provider::ProviderRepository::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    let model = provider_repo
        .get_model(model_id)
        .map_err(|error| error.to_string())?;
    if !model.enabled {
        return Err("role selected model is disabled".to_string());
    }
    if matches!(
        model.health,
        opensks_contracts::HealthState::Unavailable | opensks_contracts::HealthState::OpenCircuit
    ) {
        return Err("role selected model health is unavailable".to_string());
    }
    if !model.capabilities.satisfies(&item.capability_requirements) {
        return Err("role selected model no longer satisfies capabilities".to_string());
    }
    if let Some(provider_selector) = item.provider_selector.as_deref()
        && provider_selector != model.provider_id
    {
        return Err("role provider selector does not match selected model".to_string());
    }
    let connection = provider_repo
        .get_connection(&model.provider_id)
        .map_err(|error| error.to_string())?;
    if !connection.enabled {
        return Err("role selected provider is disabled".to_string());
    }
    if matches!(
        connection.health.state,
        opensks_contracts::HealthState::Unavailable | opensks_contracts::HealthState::OpenCircuit
    ) || connection.health.circuit_open
    {
        return Err("role selected provider health is unavailable".to_string());
    }
    if !supports_openai_compatible_dispatch(connection.kind) {
        return Err(format!(
            "provider kind `{:?}` is not supported by role dispatch",
            connection.kind
        ));
    }
    let bearer_token = resolve_provider_secret_for_dispatch(&connection.auth)?;
    Ok(RoleProviderDispatchTarget {
        connection,
        model,
        bearer_token,
    })
}

fn role_worker_model_call_body(
    remote_model_id: &str,
    max_tokens: u32,
    role: &str,
    context: &RoleWorkerExecutionContext<'_>,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_context: Option<&WorkerContextPackMaterialization>,
    reasoning_effort: ProviderReasoningEffort,
) -> serde_json::Value {
    let response_instruction = if role == "verification" {
        "Return the first line exactly as `VERDICT: pass` or `VERDICT: fail`, then at most two short bullets. Use fail for unsafe, unverified, or semantically mismatched candidates."
    } else {
        "Return at most three short bullets."
    };
    let mut body = serde_json::json!({
        "model": remote_model_id,
        "max_tokens": max_tokens.clamp(1, 256),
        "temperature": 0,
        "messages": [
            {
                "role": "system",
                "content": "You are a bounded OpenSKS role worker. Do not call tools or claim file edits. Return a concise readiness/risk assessment for this role only."
            },
            {
                "role": "user",
                "content": format!(
                    "Role: {role}\nWork item: {}\nRoot context pack ref: {}\nWorker context pack ref: {}\nWorker context:\n{}\n\nUser objective:\n{}\n\n{}",
                    item.id,
                    item.context_pack_ref.as_deref().unwrap_or("none"),
                    worker_context_ref(item, worker_context),
                    worker_context_body(worker_context),
                    context.prompt,
                    response_instruction
                )
            }
        ]
    });
    add_provider_reasoning_effort(&mut body, reasoning_effort);
    body
}

fn worker_context_ref(
    item: &opensks_contracts::SchedulerWorkItem,
    worker_context: Option<&WorkerContextPackMaterialization>,
) -> String {
    worker_context
        .map(|context| context.artifact_ref.clone())
        .or_else(|| item.worker_context_pack_ref.clone())
        .unwrap_or_else(|| "none".to_string())
}

fn worker_context_body(worker_context: Option<&WorkerContextPackMaterialization>) -> String {
    worker_context
        .map(|context| context.body.clone())
        .unwrap_or_else(|| "[worker context unavailable]".to_string())
}

fn stable_text_digest(content: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv64:{hash:016x}")
}

fn role_worker_agent_event(
    request: &opensks_adapter::AgentRunRequest,
    item: &opensks_contracts::SchedulerWorkItem,
    worker_id: &str,
    sequence: u64,
    kind: opensks_contracts::AgentEventKind,
    payload: serde_json::Value,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some(worker_id.to_string()),
        node_id: Some(item.node_id.clone()),
        sequence,
        occurred_at_ms: request.now_ms.saturating_add(sequence),
        kind,
        payload,
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: vec![
            "daemon:role-worker-executed".to_string(),
            "daemon:role-worker-model-call".to_string(),
            "scheduler:role-plan-work-item".to_string(),
            "provider:role-routing".to_string(),
        ],
    }
}

fn role_worker_sequence_base(item: &opensks_contracts::SchedulerWorkItem) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in item.id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    10_000 + (hash % 1_000_000).saturating_mul(10)
}

fn append_apply_evidence_refs(
    evidence_refs: &mut Vec<String>,
    outcome: &opensks_adapter::AgentRunOutcome,
) {
    for result in &outcome.apply_results {
        for evidence_ref in &result.evidence_refs {
            if evidence_ref.starts_with("patch-engine:")
                && !evidence_refs
                    .iter()
                    .any(|existing| existing == evidence_ref)
            {
                evidence_refs.push(evidence_ref.clone());
            }
        }
    }
}

fn dispatch_claimed_turn_via_scheduler(
    execution: ClaimedTurnExecution<'_>,
) -> Result<
    (
        opensks_adapter::AgentRunOutcome,
        opensks_contracts::RoutingDecision,
    ),
    String,
> {
    let settings_digest = execution
        .repo
        .turn_settings_digest(&execution.lease.turn_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "accepted turn has no settings digest".to_string())?;
    let mut store = opensks_event_store::EventStore::open_workspace(execution.workspace)
        .map_err(|error| error.to_string())?;
    let context_pack_ref =
        scheduler_root_context_pack_ref(&store, &execution.lease.run_id, &execution.lease.turn_id)?;
    let resource_limits =
        turn_scheduler_resource_limits_from_registry(execution.workspace, execution.settings);
    let role_plan = turn_scheduler_role_plan_for_settings(execution.workspace, execution.settings);
    let objective_plan = claimed_turn_objective_plan(
        execution.workspace,
        execution.settings,
        &execution.lease.run_id,
        execution.prompt,
    )?;
    let bootstrap = opensks_scheduler::bootstrap_conversation_turn_scheduler(
        &mut store,
        opensks_scheduler::ConversationTurnSchedulerInput {
            run_id: &execution.lease.run_id,
            turn_id: &execution.lease.turn_id,
            project_id: &execution.lease.project_id,
            conversation_id: &execution.lease.conversation_id,
            settings: execution.settings,
            settings_digest: &settings_digest,
            context_pack_ref: context_pack_ref.as_deref(),
            resource_limits,
            role_plan,
            objective_plan,
            now_ms: execution.now_ms,
        },
    )
    .map_err(|error| error.to_string())?;
    let mut scheduler = opensks_scheduler::DurableScheduler::new(
        &execution.lease.run_id,
        bootstrap.work_items,
        opensks_scheduler::conversation_turn_scheduler_config_with_limits(
            execution.settings,
            resource_limits,
        ),
    );
    let mut worker = ClaimedTurnSchedulerWorker {
        execution,
        root_outcome: None,
        result: None,
    };
    let report = scheduler
        .dispatch_ready_batch(&mut store, &mut worker)
        .map_err(|error| error.to_string())?;
    if report.attempted != 1 {
        return Err(format!(
            "conversation turn scheduler dispatched {} work items, expected 1",
            report.attempted
        ));
    }
    let root_result = worker
        .result
        .take()
        .ok_or_else(|| "conversation turn scheduler worker produced no result".to_string())?;
    let should_dispatch_role_children = root_result
        .as_ref()
        .map(|(outcome, _)| {
            outcome.final_state == opensks_contracts::projection::RunProjectionState::Completed
        })
        .unwrap_or(false);
    if should_dispatch_role_children {
        let (_snapshot, child_report) = scheduler
            .dispatch_until_idle(&mut store, &mut worker)
            .map_err(|error| error.to_string())?;
        if child_report.failed > 0 {
            return Err(format!(
                "conversation turn post-root workers failed: {} failed of {} attempted",
                child_report.failed, child_report.attempted
            ));
        }
    }
    root_result
}

fn scheduler_root_context_pack_ref(
    store: &opensks_event_store::EventStore,
    run_id: &str,
    turn_id: &str,
) -> Result<Option<String>, String> {
    let root_id = opensks_scheduler::conversation_turn_root_work_item_id(turn_id);
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    Ok(events
        .into_iter()
        .filter(|event| event.kind == opensks_contracts::EventKind::WorkItemQueued)
        .filter(|event| {
            event
                .payload
                .get("work_item_id")
                .and_then(serde_json::Value::as_str)
                == Some(root_id.as_str())
        })
        .max_by_key(|event| event.sequence)
        .and_then(|event| {
            event
                .payload
                .get("work_item")
                .and_then(|work_item| work_item.get("context_pack_ref"))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    event
                        .payload
                        .get("context_pack_ref")
                        .and_then(serde_json::Value::as_str)
                })
                .map(str::to_string)
        }))
}

fn run_claimed_turn_adapter_workload(
    execution: &ClaimedTurnExecution<'_>,
    request: &opensks_adapter::AgentRunRequest,
) -> Result<
    (
        opensks_adapter::AgentRunOutcome,
        opensks_contracts::RoutingDecision,
    ),
    String,
> {
    let selected_model_id = execution
        .initial_routing_decision
        .status
        .has_resolved_model()
        .then(|| execution.initial_routing_decision.selected_model_id.clone())
        .flatten();
    let mut final_routing_decision = execution.initial_routing_decision.clone();
    let outcome = if selected_model_id.is_some() {
        match prepare_provider_dispatch(
            execution.repo,
            execution.workspace,
            execution.lease,
            execution.initial_routing_decision,
            execution.now_ms,
        ) {
            Ok(dispatch) => {
                final_routing_decision = dispatch.routing_decision.clone();
                match opensks_adapter::OpenAiCompatibleChatCompleter::new(
                    dispatch.connection.endpoint.base_url.clone(),
                    dispatch.bearer_token.clone(),
                ) {
                    Ok(completer) => {
                        let image_executor =
                            prepare_image_tool_executor(execution.workspace, Some(execution.settings))
                                .ok();
                        let mut runtime_agentic_config = execution.agentic_config.clone();
                        let tools = if let Some(image_executor) = image_executor.as_ref() {
                            let image_tool_names = image_executor.available_tool_names();
                            allow_image_tools(&mut runtime_agentic_config, &image_tool_names);
                            opensks_adapter::tool_definitions_with_extra_available_tools(
                                &image_tool_names,
                            )
                        } else {
                            opensks_adapter::tool_definitions()
                        };
                        let completer = DispatchRecordingCompleter::new(
                            completer,
                            execution.workspace.to_path_buf(),
                            execution.lease.turn_id.clone(),
                            dispatch.routing_decision,
                        );
                        let mut driver = opensks_adapter::OpenRouterToolDriver::new(
                            dispatch.model.remote_model_id.clone(),
                            max_output_tokens_for_model(&dispatch.model),
                            completer,
                            "You are a coding agent. Use workspace tools for file changes; final text alone must not claim files changed.",
                            execution.prompt,
                        )
                        .with_chat_reasoning_effort_if_some(
                            chat_reasoning_effort_wire_for_provider(dispatch.connection.kind),
                            execution.agentic_config.reasoning_effort,
                        )
                        .with_tools(tools);
                        if let Some(image_executor) = image_executor.as_ref() {
                            opensks_adapter::run_agentic_loop_with_image_tools(
                                request,
                                &mut driver,
                                &runtime_agentic_config,
                                execution.sink,
                                image_executor,
                            )
                        } else {
                            opensks_adapter::run_agentic_loop(
                                request,
                                &mut driver,
                                &runtime_agentic_config,
                                execution.sink,
                            )
                        }
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => {
                final_routing_decision = match persist_routing_status(
                    execution.repo,
                    &execution.lease.turn_id,
                    execution.initial_routing_decision.clone(),
                    opensks_contracts::RoutingStatus::BlockedPolicy,
                    "provider_dispatch_unavailable",
                    execution.now_ms,
                ) {
                    Ok(decision) => decision,
                    Err(persist_error) => return Err(persist_error.to_string()),
                };
                opensks_adapter::AgentEventSink::emit(
                    execution.sink,
                    provider_dispatch_unavailable_agent_event(request, &error),
                );
                Ok(opensks_adapter::AgentRunOutcome {
                    assistant_text: format!("Provider dispatch is unavailable: {error}"),
                    patches: vec![],
                    apply_results: vec![],
                    final_state: opensks_contracts::projection::RunProjectionState::Failed,
                })
            }
        }
    } else if execution.explicit_local_test {
        if matches!(
            execution.settings.execution_mode,
            opensks_contracts::ExecutionMode::ReadOnly
        ) {
            opensks_adapter::AgentEventSink::emit(
                execution.sink,
                read_only_execution_mode_agent_event(request),
            );
            Ok(opensks_adapter::AgentRunOutcome {
                assistant_text:
                    "Read-only execution mode blocked workspace writes for this turn."
                        .to_string(),
                patches: vec![],
                apply_results: vec![],
                final_state: opensks_contracts::projection::RunProjectionState::Failed,
            })
        } else {
            run_explicit_local_test_turn(request, execution.sink)
        }
    } else {
        opensks_adapter::AgentEventSink::emit(execution.sink, setup_required_agent_event(request));
        Ok(opensks_adapter::AgentRunOutcome {
            assistant_text: "Needs setup — connect at least one code-capable model.".to_string(),
            patches: vec![],
            apply_results: vec![],
            final_state: opensks_contracts::projection::RunProjectionState::Failed,
        })
    }
    .map_err(|error| error.to_string())?;
    Ok((outcome, final_routing_decision))
}

type IntegrationCandidateReceipt = opensks_contracts::IntegrationCandidateReceipt;
type IntegrationCandidateSelectionReceipt = opensks_contracts::IntegrationCandidateSelectionReceipt;

#[derive(Debug, Clone)]
struct PreparedIntegrationCandidate {
    receipt: IntegrationCandidateReceipt,
    assistant_text: String,
}

#[derive(Debug, Clone)]
struct IntegrationCandidateSource {
    source: String,
    id: String,
    receipt_ref: String,
    patch_ref: String,
    worker_id: String,
    work_item_id: Option<String>,
    role: Option<String>,
    target_paths: Vec<String>,
    applied_files: Vec<String>,
    patch_count: usize,
    apply_result_count: usize,
    source_isolation_id: Option<String>,
    source_isolation_mode: Option<String>,
    source_base_commit: Option<String>,
    source_git_available: bool,
    patch_text: String,
    shard_policy_id: Option<String>,
    shard_policy_selection_policy: Option<String>,
    planner_required_source_candidate_count: usize,
    planner_required_verifier_count: usize,
}

impl IntegrationCandidateSource {
    fn source_ref(&self) -> opensks_contracts::IntegrationSourceCandidateRef {
        opensks_contracts::IntegrationSourceCandidateRef {
            source: self.source.clone(),
            id: self.id.clone(),
            receipt_ref: self.receipt_ref.clone(),
            patch_ref: self.patch_ref.clone(),
            worker_id: self.worker_id.clone(),
            work_item_id: self.work_item_id.clone(),
            role: self.role.clone(),
            source_isolation_id: self.source_isolation_id.clone(),
            source_isolation_mode: self.source_isolation_mode.clone(),
            target_paths: self.target_paths.clone(),
            shard_policy_id: self.shard_policy_id.clone(),
            shard_policy_selection_policy: self.shard_policy_selection_policy.clone(),
            planner_required_source_candidate_count: self.planner_required_source_candidate_count,
            planner_required_verifier_count: self.planner_required_verifier_count,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct IntegrationPlannerShardSelection {
    shard_policy_id: Option<String>,
    shard_policy_selection_policy: Option<String>,
    required_source_candidate_count: usize,
    selected_source_candidate_count: usize,
    required_verifier_count: usize,
}

fn integration_planner_shard_selection(
    sources: &[IntegrationCandidateSource],
) -> Result<IntegrationPlannerShardSelection, String> {
    let mut policy_ids = BTreeSet::new();
    for source in sources {
        if let Some(policy_id) = source
            .shard_policy_id
            .as_deref()
            .filter(|policy_id| !policy_id.is_empty())
        {
            policy_ids.insert(policy_id.to_string());
        }
    }
    let Some(policy_id) = policy_ids.iter().next().cloned() else {
        return Ok(IntegrationPlannerShardSelection::default());
    };
    if policy_ids.len() > 1 {
        return Err("planner_shard_policy_conflict".to_string());
    }
    let mut selection_policy = None;
    let mut required_source_candidate_count = 0usize;
    let mut required_verifier_count = 0usize;
    let mut selected_source_candidate_count = 0usize;
    for source in sources {
        if source.shard_policy_id.as_deref() != Some(policy_id.as_str()) {
            continue;
        }
        selected_source_candidate_count = selected_source_candidate_count.saturating_add(1);
        if selection_policy.is_none() {
            selection_policy = source.shard_policy_selection_policy.clone();
        }
        required_source_candidate_count =
            required_source_candidate_count.max(source.planner_required_source_candidate_count);
        required_verifier_count =
            required_verifier_count.max(source.planner_required_verifier_count);
    }
    Ok(IntegrationPlannerShardSelection {
        shard_policy_id: Some(policy_id),
        shard_policy_selection_policy: selection_policy,
        required_source_candidate_count,
        selected_source_candidate_count,
        required_verifier_count,
    })
}

fn candidate_missing_planner_required_shards(candidate: &IntegrationCandidateReceipt) -> bool {
    candidate.shard_policy_id.is_some()
        && candidate_missing_planner_required_shards_candidate_counts(
            candidate.planner_required_source_candidate_count,
            candidate.planner_selected_source_candidate_count,
        )
}

fn candidate_planner_verifier_count_exceeds_runtime_cap(
    candidate: &IntegrationCandidateReceipt,
) -> bool {
    candidate.shard_policy_id.is_some()
        && candidate.planner_required_verifier_count > MAX_INTEGRATION_VERIFIER_LANES
}

fn candidate_missing_planner_required_shards_candidate_counts(
    required_source_candidate_count: usize,
    selected_source_candidate_count: usize,
) -> bool {
    required_source_candidate_count > 0
        && selected_source_candidate_count < required_source_candidate_count
}

fn write_integration_candidate(
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
    settings: &opensks_contracts::ConversationTurnSettings,
    execution_workspace: &TurnExecutionWorkspace,
    outcome: &opensks_adapter::AgentRunOutcome,
    now_ms: u64,
) -> Result<Option<PreparedIntegrationCandidate>, String> {
    let candidate_id = format!("integration-candidate-{}", lease.run_id);
    let relative_dir = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(&lease.run_id);
    let dir = workspace.join(&relative_dir);
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let patch_relative = relative_dir.join("candidate.patch");
    let receipt_relative = relative_dir.join("candidate.json");
    let selection_relative = relative_dir.join("selection.json");
    let patch_ref = artifact_ref(&patch_relative);
    let receipt_ref = artifact_ref(&receipt_relative);
    let selection_ref = artifact_ref(&selection_relative);

    let mut sources = Vec::new();
    if let Some(source) = turn_supervisor_candidate_source(
        lease,
        execution_workspace,
        outcome,
        &receipt_ref,
        &patch_ref,
    ) {
        sources.push(source);
    }
    sources.extend(role_subcontract_candidate_sources(
        workspace,
        &lease.run_id,
    )?);
    if sources.is_empty() {
        return Ok(None);
    }

    let mut target_paths = std::collections::BTreeSet::new();
    let mut applied_files = std::collections::BTreeSet::new();
    let mut patch_count = 0usize;
    let mut apply_result_count = 0usize;
    for source in &sources {
        target_paths.extend(source.target_paths.iter().cloned());
        applied_files.extend(source.applied_files.iter().cloned());
        patch_count = patch_count.saturating_add(source.patch_count);
        apply_result_count = apply_result_count.saturating_add(source.apply_result_count);
    }
    let target_paths = target_paths.into_iter().collect::<Vec<_>>();
    if target_paths.is_empty() {
        return Ok(None);
    }
    let applied_files = applied_files.into_iter().collect::<Vec<_>>();
    let unified_diff = sources
        .iter()
        .map(|source| source.patch_text.trim_end())
        .filter(|patch| !patch.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if unified_diff.trim().is_empty() {
        return Ok(None);
    }
    std::fs::write(dir.join("candidate.patch"), format!("{unified_diff}\n"))
        .map_err(|error| error.to_string())?;

    let assistant_text = format!(
        "Prepared isolated change candidate for {} from {} source candidate(s). The main workspace was not modified; integration is pending verification and approval.",
        target_paths.join(", "),
        sources.len()
    );
    let source_metadata = sources
        .iter()
        .find(|source| source.source_isolation_id.is_some())
        .unwrap_or(&sources[0]);
    let reason_code = if sources.len() > 1 {
        "aggregate_isolated_patch_candidate_ready"
    } else if sources[0].source == "role_subcontract" {
        "role_candidate_aggregate_ready"
    } else {
        "isolated_patch_candidate_ready"
    };
    let mut evidence_refs = vec![
        "integration:candidate-ready".to_string(),
        "patch-engine:atomic-apply".to_string(),
    ];
    if sources
        .iter()
        .any(|source| source.source.as_str() == "turn_supervisor")
    {
        evidence_refs.push("git:isolation-prepared".to_string());
    }
    if sources
        .iter()
        .any(|source| source.source.as_str() == "role_subcontract")
    {
        evidence_refs.push("daemon:role-worker-code-candidate".to_string());
        evidence_refs.push("integration:role-candidate-aggregate".to_string());
    }
    if sources.len() > 1 {
        evidence_refs.push("integration:aggregate-candidate-ready".to_string());
    }
    evidence_refs.push("integration:candidate-selection-receipt".to_string());
    evidence_refs.push("settings:approval-policy".to_string());
    evidence_refs.push("settings:turn-settings-snapshot".to_string());
    let source_candidates = sources
        .iter()
        .map(IntegrationCandidateSource::source_ref)
        .collect::<Vec<_>>();
    let selected_source_candidate_ids = source_candidates
        .iter()
        .map(|source| source.id.clone())
        .collect::<Vec<_>>();
    let planner_shard_selection = integration_planner_shard_selection(&sources)?;
    if planner_shard_selection.shard_policy_id.is_some() {
        evidence_refs.push("planner:shard-policy".to_string());
        evidence_refs.push("integration:planner-shard-selection".to_string());
    }
    let selection_policy = if let Some(policy) = planner_shard_selection
        .shard_policy_selection_policy
        .as_deref()
    {
        policy
    } else if sources.len() > 1 {
        "deterministic_all_ready_source_candidates"
    } else {
        "deterministic_single_ready_candidate"
    };
    let selection_reason_code = if candidate_missing_planner_required_shards_candidate_counts(
        planner_shard_selection.required_source_candidate_count,
        planner_shard_selection.selected_source_candidate_count,
    ) {
        "planner_required_shards_missing"
    } else if planner_shard_selection.shard_policy_id.is_some() {
        "planner_required_shards_selected"
    } else if sources.len() > 1 {
        "aggregate_ready_sources_selected"
    } else {
        "single_ready_source_selected"
    };
    let planned_verifier_count = planner_shard_selection
        .required_verifier_count
        .max(planned_integration_verifier_count(settings.verifier_count))
        .min(MAX_INTEGRATION_VERIFIER_LANES);
    let mut selection_evidence_refs = vec![
        "integration:candidate-selection-receipt".to_string(),
        "schema:integration-candidate-selection-receipt".to_string(),
        "settings:approval-policy".to_string(),
        "settings:turn-settings-snapshot".to_string(),
    ];
    if planner_shard_selection.shard_policy_id.is_some() {
        selection_evidence_refs.push("planner:shard-policy".to_string());
        selection_evidence_refs.push("integration:planner-shard-selection".to_string());
    }
    let receipt = IntegrationCandidateReceipt {
        schema: opensks_contracts::INTEGRATION_CANDIDATE_RECEIPT_SCHEMA.to_string(),
        id: candidate_id.clone(),
        run_id: lease.run_id.clone(),
        turn_id: lease.turn_id.clone(),
        conversation_id: lease.conversation_id.clone(),
        project_id: lease.project_id.clone(),
        worker_id: "integration-coordinator".to_string(),
        state: "candidate_ready".to_string(),
        reason_code: reason_code.to_string(),
        source_isolation_id: source_metadata.source_isolation_id.clone(),
        source_isolation_mode: source_metadata.source_isolation_mode.clone(),
        source_base_commit: source_metadata.source_base_commit.clone(),
        source_git_available: sources.iter().any(|source| source.source_git_available),
        source_candidates: source_candidates.clone(),
        aggregate_candidate_count: sources.len(),
        aggregate_target_count: target_paths.len(),
        planned_verifier_count,
        shard_policy_id: planner_shard_selection.shard_policy_id.clone(),
        shard_policy_selection_policy: planner_shard_selection
            .shard_policy_selection_policy
            .clone(),
        planner_required_source_candidate_count: planner_shard_selection
            .required_source_candidate_count,
        planner_selected_source_candidate_count: planner_shard_selection
            .selected_source_candidate_count,
        planner_required_verifier_count: planner_shard_selection.required_verifier_count,
        target_paths: target_paths.clone(),
        patch_count,
        apply_result_count,
        applied_files,
        receipt_ref: receipt_ref.clone(),
        patch_ref: patch_ref.clone(),
        selection_ref: Some(selection_ref.clone()),
        main_workspace_modified: false,
        integration_required: true,
        approval_required: true,
        approval_policy_id: Some(settings.approval_policy_id.clone()),
        turn_settings: Some(opensks_contracts::IntegrationTurnSettingsSnapshot::from(
            settings,
        )),
        path_redacted: true,
        content_redacted: true,
        generated_at_ms: now_ms,
        evidence_refs: evidence_refs.clone(),
    };
    let selection = IntegrationCandidateSelectionReceipt {
        schema: opensks_contracts::INTEGRATION_CANDIDATE_SELECTION_RECEIPT_SCHEMA.to_string(),
        id: format!("integration-candidate-selection-{}", lease.run_id),
        run_id: lease.run_id.clone(),
        selected_candidate_id: candidate_id,
        selection_ref: selection_ref.clone(),
        selected_candidate_ref: receipt_ref.clone(),
        selected_patch_ref: patch_ref.clone(),
        candidate_pool: source_candidates,
        selected_source_candidate_ids,
        selection_policy: selection_policy.to_string(),
        reason_code: selection_reason_code.to_string(),
        required_verification_gates: vec![
            "candidate_receipt_valid".to_string(),
            "target_policy_check".to_string(),
            "patch_apply_check".to_string(),
            "read_only_verifier_lanes".to_string(),
            "approval_event".to_string(),
        ],
        aggregate_candidate_count: sources.len(),
        aggregate_target_count: target_paths.len(),
        planned_verifier_count,
        approval_policy_id: Some(settings.approval_policy_id.clone()),
        turn_settings: Some(opensks_contracts::IntegrationTurnSettingsSnapshot::from(
            settings,
        )),
        shard_policy_id: planner_shard_selection.shard_policy_id.clone(),
        shard_policy_selection_policy: planner_shard_selection
            .shard_policy_selection_policy
            .clone(),
        planner_required_source_candidate_count: planner_shard_selection
            .required_source_candidate_count,
        planner_selected_source_candidate_count: planner_shard_selection
            .selected_source_candidate_count,
        planner_required_verifier_count: planner_shard_selection.required_verifier_count,
        target_paths,
        path_redacted: true,
        content_redacted: true,
        evidence_refs: selection_evidence_refs,
        generated_at_ms: now_ms,
    };
    std::fs::write(
        dir.join("candidate.json"),
        serde_json::to_string_pretty(&receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(
        dir.join("selection.json"),
        serde_json::to_string_pretty(&selection).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    Ok(Some(PreparedIntegrationCandidate {
        receipt,
        assistant_text,
    }))
}

fn planned_integration_verifier_count(verifier_count: u32) -> usize {
    (verifier_count.max(1) as usize).min(MAX_INTEGRATION_VERIFIER_LANES)
}

fn turn_supervisor_candidate_source(
    lease: &opensks_conversation::TurnSupervisorLease,
    execution_workspace: &TurnExecutionWorkspace,
    outcome: &opensks_adapter::AgentRunOutcome,
    receipt_ref: &str,
    patch_ref: &str,
) -> Option<IntegrationCandidateSource> {
    let isolation = execution_workspace.isolation.as_ref()?;
    if !outcome.apply_results.iter().any(|result| result.applied) {
        return None;
    }
    let mut target_paths = std::collections::BTreeSet::new();
    for patch in &outcome.patches {
        for file in &patch.files {
            target_paths.insert(file.path.clone());
        }
    }
    if target_paths.is_empty() {
        return None;
    }
    let patch_text = outcome
        .patches
        .iter()
        .flat_map(|patch| patch.files.iter())
        .map(|file| file.unified_diff.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if patch_text.trim().is_empty() {
        return None;
    }
    let applied_files = outcome
        .apply_results
        .iter()
        .flat_map(|result| result.applied_files.iter().cloned())
        .collect::<Vec<_>>();
    Some(IntegrationCandidateSource {
        source: "turn_supervisor".to_string(),
        id: format!("turn-supervisor-candidate-{}", lease.run_id),
        receipt_ref: receipt_ref.to_string(),
        patch_ref: patch_ref.to_string(),
        worker_id: "turn-supervisor".to_string(),
        work_item_id: None,
        role: None,
        target_paths: target_paths.into_iter().collect(),
        applied_files,
        patch_count: outcome.patches.len(),
        apply_result_count: outcome.apply_results.len(),
        source_isolation_id: Some(isolation.id.clone()),
        source_isolation_mode: Some(isolation_mode_label(&isolation.mode).to_string()),
        source_base_commit: isolation.base_commit.clone(),
        source_git_available: isolation.git_available,
        patch_text,
        shard_policy_id: None,
        shard_policy_selection_policy: None,
        planner_required_source_candidate_count: 0,
        planner_required_verifier_count: 0,
    })
}

fn role_subcontract_candidate_sources(
    workspace: &Path,
    run_id: &str,
) -> Result<Vec<IntegrationCandidateSource>, String> {
    let role_root = workspace
        .join(".opensks")
        .join("runtime")
        .join("role-candidates")
        .join(run_id);
    let entries = match std::fs::read_dir(&role_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut candidate_dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            candidate_dirs.push(entry.path());
        }
    }
    candidate_dirs.sort();

    let mut sources = Vec::new();
    for candidate_dir in candidate_dirs {
        let raw = std::fs::read_to_string(candidate_dir.join("candidate.json"))
            .map_err(|error| error.to_string())?;
        let receipt: RoleSubcontractCandidateReceipt =
            serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        if receipt.schema != opensks_contracts::ROLE_SUBCONTRACT_CANDIDATE_RECEIPT_SCHEMA
            || receipt.state != "candidate_ready"
            || !receipt.integration_required
        {
            continue;
        }
        let patch_text = std::fs::read_to_string(candidate_dir.join("candidate.patch"))
            .map_err(|error| error.to_string())?;
        if patch_text.trim().is_empty() || receipt.target_paths.is_empty() {
            continue;
        }
        sources.push(IntegrationCandidateSource {
            source: "role_subcontract".to_string(),
            id: receipt.id,
            receipt_ref: receipt.receipt_ref,
            patch_ref: receipt.patch_ref,
            worker_id: receipt.worker_id,
            work_item_id: Some(receipt.work_item_id),
            role: Some(receipt.role),
            target_paths: receipt.target_paths,
            applied_files: receipt.applied_files,
            patch_count: receipt.patch_count,
            apply_result_count: receipt.apply_result_count,
            source_isolation_id: Some(receipt.source_isolation_id),
            source_isolation_mode: Some(receipt.source_isolation_mode),
            source_base_commit: receipt.source_base_commit,
            source_git_available: receipt.source_git_available,
            patch_text,
            shard_policy_id: receipt.shard_policy_id,
            shard_policy_selection_policy: receipt.shard_policy_selection_policy,
            planner_required_source_candidate_count: receipt
                .planner_required_source_candidate_count,
            planner_required_verifier_count: receipt.planner_required_verifier_count,
        });
    }
    Ok(sources)
}

fn integration_candidate_apply_lines(
    request: &EngineRequest,
    options: &DaemonOptions,
) -> Result<Vec<String>, DaemonError> {
    let Some(run_id) = request.params.run_id.as_deref() else {
        return Ok(vec![serde_json::to_string(&missing_param_event(
            &request.id,
            "run_id",
        ))?]);
    };
    let receipt = apply_integration_candidate(
        &options.workspace,
        run_id,
        request.params.approval_id.as_deref(),
        timestamp_ms(),
    )
    .map_err(|error| DaemonError::Io(std::io::Error::other(error)))?;
    append_integration_apply_event(&options.workspace, &receipt)
        .map_err(|error| DaemonError::Io(std::io::Error::other(error)))?;
    Ok(vec![serde_json::to_string(&receipt)?])
}

fn apply_integration_candidate(
    workspace: &Path,
    run_id: &str,
    approval_id: Option<&str>,
    now_ms: u64,
) -> Result<opensks_contracts::IntegrationApplyReceipt, String> {
    let candidate_dir = workspace
        .join(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id);
    std::fs::create_dir_all(&candidate_dir).map_err(|error| error.to_string())?;
    let candidate_relative = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join("candidate.json");
    let patch_relative = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join("candidate.patch");
    let integration_relative = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join("integration.json");
    let verification_relative = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join("verification.json");
    let final_diff_relative = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join("final.diff");
    let seal_relative = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join("seal.json");
    let repair_relative = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join("repair.json");
    let cleanup_relative = PathBuf::from(".opensks")
        .join("runtime")
        .join("integration-candidates")
        .join(run_id)
        .join("cleanup.json");
    let candidate_ref = artifact_ref(&candidate_relative);
    let patch_ref = artifact_ref(&patch_relative);
    let integration_ref = artifact_ref(&integration_relative);
    let verification_ref = artifact_ref(&verification_relative);
    let final_diff_ref = artifact_ref(&final_diff_relative);
    let seal_ref = artifact_ref(&seal_relative);
    let repair_ref = artifact_ref(&repair_relative);
    let cleanup_ref = artifact_ref(&cleanup_relative);
    let expected_approval_id = format!("approval-integration-{run_id}");

    let candidate_raw = match std::fs::read_to_string(candidate_dir.join("candidate.json")) {
        Ok(raw) => raw,
        Err(error) => {
            let verification =
                integration_verification_receipt(IntegrationVerificationReceiptInput {
                    run_id,
                    candidate_id: "",
                    state: "failed",
                    reason_code: "candidate_receipt_missing",
                    target_paths: Vec::new(),
                    passed_gates: Vec::new(),
                    failed_gates: vec!["candidate_receipt_missing".to_string()],
                    candidate_ref: &candidate_ref,
                    patch_ref: &patch_ref,
                    verification_ref: &verification_ref,
                    repair_ref: None,
                    planned_verifier_count: 0,
                    verifier_lanes: empty_verifier_lanes(),
                    generated_at_ms: now_ms,
                });
            write_integration_verification_receipt(&candidate_dir, &verification)?;
            let receipt = integration_apply_receipt(IntegrationApplyReceiptInput {
                run_id,
                candidate_id: "",
                state: "failed",
                reason_code: "candidate_receipt_missing",
                target_paths: Vec::new(),
                approval_id: Some(expected_approval_id),
                approval_policy_id: None,
                turn_settings: None,
                candidate_ref: &candidate_ref,
                patch_ref: &patch_ref,
                verification_ref: Some(&verification_ref),
                integration_ref: &integration_ref,
                final_diff_ref: &final_diff_ref,
                seal_ref: None,
                repair_ref: None,
                cleanup_ref: None,
                main_workspace_modified: false,
                verifier_passed: false,
                generated_at_ms: now_ms,
            });
            write_integration_apply_receipt(&candidate_dir, &receipt)?;
            return Err(format!("candidate receipt missing: {error}"));
        }
    };
    let candidate: IntegrationCandidateReceipt = match serde_json::from_str(&candidate_raw) {
        Ok(candidate) => candidate,
        Err(error) => {
            let verification =
                integration_verification_receipt(IntegrationVerificationReceiptInput {
                    run_id,
                    candidate_id: "",
                    state: "failed",
                    reason_code: "candidate_receipt_invalid",
                    target_paths: Vec::new(),
                    passed_gates: Vec::new(),
                    failed_gates: vec!["candidate_receipt_invalid".to_string()],
                    candidate_ref: &candidate_ref,
                    patch_ref: &patch_ref,
                    verification_ref: &verification_ref,
                    repair_ref: None,
                    planned_verifier_count: 0,
                    verifier_lanes: empty_verifier_lanes(),
                    generated_at_ms: now_ms,
                });
            write_integration_verification_receipt(&candidate_dir, &verification)?;
            let receipt = integration_apply_receipt(IntegrationApplyReceiptInput {
                run_id,
                candidate_id: "",
                state: "failed",
                reason_code: "candidate_receipt_invalid",
                target_paths: Vec::new(),
                approval_id: Some(expected_approval_id),
                approval_policy_id: None,
                turn_settings: None,
                candidate_ref: &candidate_ref,
                patch_ref: &patch_ref,
                verification_ref: Some(&verification_ref),
                integration_ref: &integration_ref,
                final_diff_ref: &final_diff_ref,
                seal_ref: None,
                repair_ref: None,
                cleanup_ref: None,
                main_workspace_modified: false,
                verifier_passed: false,
                generated_at_ms: now_ms,
            });
            write_integration_apply_receipt(&candidate_dir, &receipt)?;
            return Err(format!("candidate receipt invalid: {error}"));
        }
    };
    let candidate_id = candidate.id.clone();
    let target_paths = candidate.target_paths.clone();
    let approval_policy_id = candidate.approval_policy_id.as_deref();
    let turn_settings = candidate.turn_settings.as_ref();

    let failure = |reason_code: &str| {
        integration_apply_receipt(IntegrationApplyReceiptInput {
            run_id,
            candidate_id: &candidate_id,
            state: "failed",
            reason_code,
            target_paths: target_paths.clone(),
            approval_id: Some(expected_approval_id.clone()),
            approval_policy_id,
            turn_settings,
            candidate_ref: &candidate_ref,
            patch_ref: &patch_ref,
            verification_ref: Some(&verification_ref),
            integration_ref: &integration_ref,
            final_diff_ref: &final_diff_ref,
            seal_ref: None,
            repair_ref: None,
            cleanup_ref: None,
            main_workspace_modified: false,
            verifier_passed: false,
            generated_at_ms: now_ms,
        })
    };

    if candidate.schema != opensks_contracts::INTEGRATION_CANDIDATE_RECEIPT_SCHEMA
        || candidate.state != "candidate_ready"
    {
        let mut verification =
            integration_verification_receipt(IntegrationVerificationReceiptInput {
                run_id,
                candidate_id: &candidate_id,
                state: "failed",
                reason_code: "candidate_receipt_invalid",
                target_paths: target_paths.clone(),
                passed_gates: Vec::new(),
                failed_gates: vec!["candidate_receipt_invalid".to_string()],
                candidate_ref: &candidate_ref,
                patch_ref: &patch_ref,
                verification_ref: &verification_ref,
                repair_ref: None,
                planned_verifier_count: candidate_planned_verifier_count(&candidate),
                verifier_lanes: empty_verifier_lanes(),
                generated_at_ms: now_ms,
            });
        add_planner_shard_verification_evidence(&mut verification, &candidate);
        write_integration_verification_receipt(&candidate_dir, &verification)?;
        let receipt = failure("candidate_receipt_invalid");
        write_integration_apply_receipt(&candidate_dir, &receipt)?;
        return Ok(receipt);
    }
    if candidate_missing_planner_required_shards(&candidate) {
        let mut verification =
            integration_verification_receipt(IntegrationVerificationReceiptInput {
                run_id,
                candidate_id: &candidate_id,
                state: "failed",
                reason_code: "planner_required_shards_missing",
                target_paths: target_paths.clone(),
                passed_gates: vec!["candidate_receipt_valid".to_string()],
                failed_gates: vec!["planner_required_shards_present".to_string()],
                candidate_ref: &candidate_ref,
                patch_ref: &patch_ref,
                verification_ref: &verification_ref,
                repair_ref: None,
                planned_verifier_count: candidate_planned_verifier_count(&candidate),
                verifier_lanes: empty_verifier_lanes(),
                generated_at_ms: now_ms,
            });
        add_planner_shard_verification_evidence(&mut verification, &candidate);
        write_integration_verification_receipt(&candidate_dir, &verification)?;
        let receipt = integration_apply_receipt(IntegrationApplyReceiptInput {
            run_id,
            candidate_id: &candidate_id,
            state: "failed",
            reason_code: "planner_required_shards_missing",
            target_paths,
            approval_id: Some(expected_approval_id),
            approval_policy_id,
            turn_settings,
            candidate_ref: &candidate_ref,
            patch_ref: &patch_ref,
            verification_ref: Some(&verification_ref),
            integration_ref: &integration_ref,
            final_diff_ref: &final_diff_ref,
            seal_ref: None,
            repair_ref: None,
            cleanup_ref: None,
            main_workspace_modified: false,
            verifier_passed: false,
            generated_at_ms: now_ms,
        });
        write_integration_apply_receipt(&candidate_dir, &receipt)?;
        return Ok(receipt);
    }
    if candidate_planner_verifier_count_exceeds_runtime_cap(&candidate) {
        let mut verification =
            integration_verification_receipt(IntegrationVerificationReceiptInput {
                run_id,
                candidate_id: &candidate_id,
                state: "failed",
                reason_code: "planner_required_verifier_count_exceeds_runtime_cap",
                target_paths: target_paths.clone(),
                passed_gates: vec![
                    "candidate_receipt_valid".to_string(),
                    "planner_required_shards_present".to_string(),
                ],
                failed_gates: vec![
                    "planner_required_verifier_count_within_runtime_cap".to_string(),
                ],
                candidate_ref: &candidate_ref,
                patch_ref: &patch_ref,
                verification_ref: &verification_ref,
                repair_ref: None,
                planned_verifier_count: candidate_planned_verifier_count(&candidate),
                verifier_lanes: empty_verifier_lanes(),
                generated_at_ms: now_ms,
            });
        add_planner_shard_verification_evidence(&mut verification, &candidate);
        write_integration_verification_receipt(&candidate_dir, &verification)?;
        let receipt = integration_apply_receipt(IntegrationApplyReceiptInput {
            run_id,
            candidate_id: &candidate_id,
            state: "failed",
            reason_code: "planner_required_verifier_count_exceeds_runtime_cap",
            target_paths,
            approval_id: Some(expected_approval_id),
            approval_policy_id,
            turn_settings,
            candidate_ref: &candidate_ref,
            patch_ref: &patch_ref,
            verification_ref: Some(&verification_ref),
            integration_ref: &integration_ref,
            final_diff_ref: &final_diff_ref,
            seal_ref: None,
            repair_ref: None,
            cleanup_ref: None,
            main_workspace_modified: false,
            verifier_passed: false,
            generated_at_ms: now_ms,
        });
        write_integration_apply_receipt(&candidate_dir, &receipt)?;
        return Ok(receipt);
    }
    if target_paths.is_empty() {
        let verification = integration_verification_receipt(IntegrationVerificationReceiptInput {
            run_id,
            candidate_id: &candidate_id,
            state: "failed",
            reason_code: "candidate_has_no_targets",
            target_paths: Vec::new(),
            passed_gates: vec!["candidate_receipt_valid".to_string()],
            failed_gates: vec!["candidate_has_no_targets".to_string()],
            candidate_ref: &candidate_ref,
            patch_ref: &patch_ref,
            verification_ref: &verification_ref,
            repair_ref: None,
            planned_verifier_count: candidate_planned_verifier_count(&candidate),
            verifier_lanes: empty_verifier_lanes(),
            generated_at_ms: now_ms,
        });
        write_integration_verification_receipt(&candidate_dir, &verification)?;
        let receipt = failure("candidate_has_no_targets");
        write_integration_apply_receipt(&candidate_dir, &receipt)?;
        return Ok(receipt);
    }
    if target_paths
        .iter()
        .any(|path| invalid_integration_target(path))
    {
        let verification = integration_verification_receipt(IntegrationVerificationReceiptInput {
            run_id,
            candidate_id: &candidate_id,
            state: "failed",
            reason_code: "candidate_target_rejected",
            target_paths: target_paths.clone(),
            passed_gates: vec!["candidate_receipt_valid".to_string()],
            failed_gates: vec!["target_policy_rejected".to_string()],
            candidate_ref: &candidate_ref,
            patch_ref: &patch_ref,
            verification_ref: &verification_ref,
            repair_ref: None,
            planned_verifier_count: candidate_planned_verifier_count(&candidate),
            verifier_lanes: empty_verifier_lanes(),
            generated_at_ms: now_ms,
        });
        write_integration_verification_receipt(&candidate_dir, &verification)?;
        let receipt = failure("candidate_target_rejected");
        write_integration_apply_receipt(&candidate_dir, &receipt)?;
        return Ok(receipt);
    }

    let (verification, unified_diff) = verify_integration_candidate_patch(
        workspace,
        &candidate_dir,
        run_id,
        &candidate_id,
        &candidate,
        target_paths.clone(),
        &candidate_ref,
        &patch_ref,
        &verification_ref,
        &repair_ref,
        &integration_ref,
        now_ms,
    )?;
    if verification.state != "passed" {
        let receipt = integration_apply_receipt(IntegrationApplyReceiptInput {
            run_id,
            candidate_id: &candidate_id,
            state: "failed",
            reason_code: &verification.reason_code,
            target_paths,
            approval_id: Some(expected_approval_id),
            approval_policy_id,
            turn_settings,
            candidate_ref: &candidate_ref,
            patch_ref: &patch_ref,
            verification_ref: Some(&verification_ref),
            integration_ref: &integration_ref,
            final_diff_ref: &final_diff_ref,
            seal_ref: None,
            repair_ref: verification.repair_ref.as_deref(),
            cleanup_ref: None,
            main_workspace_modified: false,
            verifier_passed: false,
            generated_at_ms: now_ms,
        });
        write_integration_apply_receipt(&candidate_dir, &receipt)?;
        return Ok(receipt);
    }

    let approval_id_matches = approval_id == Some(expected_approval_id.as_str());
    let approval_persisted = approval_id_matches
        && persisted_integration_approval(workspace, run_id, &expected_approval_id)?;
    if !approval_persisted {
        let receipt = integration_apply_receipt(IntegrationApplyReceiptInput {
            run_id,
            candidate_id: &candidate_id,
            state: "awaiting_approval",
            reason_code: if approval_id_matches {
                "approval_not_persisted"
            } else {
                "approval_required"
            },
            target_paths,
            approval_id: Some(expected_approval_id),
            approval_policy_id,
            turn_settings,
            candidate_ref: &candidate_ref,
            patch_ref: &patch_ref,
            verification_ref: Some(&verification_ref),
            integration_ref: &integration_ref,
            final_diff_ref: &final_diff_ref,
            seal_ref: None,
            repair_ref: None,
            cleanup_ref: None,
            main_workspace_modified: false,
            verifier_passed: true,
            generated_at_ms: now_ms,
        });
        write_integration_apply_receipt(&candidate_dir, &receipt)?;
        return Ok(receipt);
    }

    let mut envelope = opensks_git::new_patch_envelope(
        format!("integration-{run_id}"),
        "integration-coordinator",
        expected_approval_id.clone(),
        target_paths.clone(),
    );
    envelope.base_commit = candidate
        .source_base_commit
        .clone()
        .filter(|value| !value.is_empty());
    envelope.unified_diff_ref = patch_ref.clone();
    envelope.rollback_ref = "memory://integration-coordinator-rollback".to_string();

    let mut final_diff = None;
    let apply_result =
        opensks_git::apply_unified_diff_with_rollback(workspace, &envelope, &unified_diff, || {
            match opensks_git::working_tree_diff(workspace, &target_paths) {
                Ok(diff) if !diff.trim().is_empty() => {
                    final_diff = Some(diff);
                    true
                }
                _ => false,
            }
        });
    let (state, reason_code, main_workspace_modified, verifier_passed, receipt_repair_ref) =
        match apply_result {
            Ok(()) => (
                "integrated",
                "candidate_applied_to_main_workspace",
                true,
                true,
                None,
            ),
            Err(error) => {
                let reason_code = integration_apply_error_reason(&error);
                let repair_ref = integration_apply_needs_repair(&error)
                    .then(|| {
                        let item = integration_repair_item(IntegrationRepairItemInput {
                            run_id,
                            candidate_id: &candidate_id,
                            reason_code,
                            target_paths: target_paths.clone(),
                            conflict_paths: target_paths.clone(),
                            candidate_ref: &candidate_ref,
                            patch_ref: &patch_ref,
                            integration_ref: &integration_ref,
                            repair_ref: &repair_ref,
                            generated_at_ms: now_ms,
                        });
                        write_integration_repair_item(&candidate_dir, &item)?;
                        Ok::<_, String>(repair_ref.clone())
                    })
                    .transpose()?;
                ("failed", reason_code, false, false, repair_ref)
            }
        };
    if let Some(diff) = final_diff.as_ref() {
        std::fs::write(candidate_dir.join("final.diff"), diff)
            .map_err(|error| error.to_string())?;
    }
    let receipt_cleanup_ref = (state == "integrated").then_some(cleanup_ref.as_str());
    let receipt_seal_ref = if state == "integrated" {
        let seal = integration_final_seal(IntegrationFinalSealInput {
            run_id,
            candidate_id: &candidate_id,
            target_paths: target_paths.clone(),
            approval_id: Some(envelope.lease_id.clone()),
            approval_policy_id,
            turn_settings,
            candidate_ref: &candidate_ref,
            patch_ref: &patch_ref,
            verification_ref: &verification_ref,
            integration_ref: &integration_ref,
            final_diff_ref: &final_diff_ref,
            seal_ref: &seal_ref,
            cleanup_ref: receipt_cleanup_ref,
            generated_at_ms: now_ms,
        });
        write_integration_final_seal(&candidate_dir, &seal)?;
        let cleanup = integration_cleanup_receipt(
            workspace,
            IntegrationCleanupReceiptInput {
                run_id,
                candidate_id: &candidate_id,
                candidate: &candidate,
                integration_ref: &integration_ref,
                seal_ref: &seal_ref,
                cleanup_ref: &cleanup_ref,
                candidate_ref: &candidate_ref,
                patch_ref: &patch_ref,
                final_diff_ref: &final_diff_ref,
                generated_at_ms: now_ms,
            },
        );
        write_integration_cleanup_receipt(&candidate_dir, &cleanup)?;
        Some(seal_ref.as_str())
    } else {
        None
    };
    let receipt = integration_apply_receipt(IntegrationApplyReceiptInput {
        run_id,
        candidate_id: &candidate_id,
        state,
        reason_code,
        target_paths,
        approval_id: Some(envelope.lease_id),
        approval_policy_id,
        turn_settings,
        candidate_ref: &candidate_ref,
        patch_ref: &patch_ref,
        verification_ref: Some(&verification_ref),
        integration_ref: &integration_ref,
        final_diff_ref: &final_diff_ref,
        seal_ref: receipt_seal_ref,
        repair_ref: receipt_repair_ref.as_deref(),
        cleanup_ref: receipt_cleanup_ref,
        main_workspace_modified,
        verifier_passed,
        generated_at_ms: now_ms,
    });
    write_integration_apply_receipt(&candidate_dir, &receipt)?;
    Ok(receipt)
}

fn persisted_integration_approval(
    workspace: &Path,
    run_id: &str,
    approval_id: &str,
) -> Result<bool, String> {
    let store = opensks_event_store::EventStore::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    let events = store.replay(run_id).map_err(|error| error.to_string())?;
    Ok(events.iter().any(|event| {
        event.kind == opensks_contracts::EventKind::ApprovalApproved
            && event.correlation_id.as_deref() == Some(approval_id)
            && event.payload["approval_id"].as_str() == Some(approval_id)
            && event.payload["scope"].as_str() == Some("integration_apply")
            && event.payload["state"].as_str() == Some("approved")
    }))
}

struct IntegrationVerificationReceiptInput<'a> {
    run_id: &'a str,
    candidate_id: &'a str,
    state: &'a str,
    reason_code: &'a str,
    target_paths: Vec<String>,
    passed_gates: Vec<String>,
    failed_gates: Vec<String>,
    candidate_ref: &'a str,
    patch_ref: &'a str,
    verification_ref: &'a str,
    repair_ref: Option<&'a str>,
    planned_verifier_count: usize,
    verifier_lanes: Vec<opensks_contracts::IntegrationVerifierLaneReceipt>,
    generated_at_ms: u64,
}

fn integration_verification_receipt(
    input: IntegrationVerificationReceiptInput<'_>,
) -> opensks_contracts::IntegrationVerificationReceipt {
    opensks_contracts::IntegrationVerificationReceipt {
        schema: opensks_contracts::INTEGRATION_VERIFICATION_RECEIPT_SCHEMA.to_string(),
        id: format!("integration-verification-{}", input.run_id),
        run_id: input.run_id.to_string(),
        candidate_id: input.candidate_id.to_string(),
        state: input.state.to_string(),
        reason_code: input.reason_code.to_string(),
        target_paths: input.target_paths,
        passed_gates: input.passed_gates,
        failed_gates: input.failed_gates,
        planned_verifier_count: input.planned_verifier_count,
        passed_verifier_count: input
            .verifier_lanes
            .iter()
            .filter(|lane| lane.state == "passed")
            .count(),
        failed_verifier_count: input
            .verifier_lanes
            .iter()
            .filter(|lane| lane.state == "failed")
            .count(),
        verifier_lanes: input.verifier_lanes,
        candidate_ref: input.candidate_ref.to_string(),
        patch_ref: input.patch_ref.to_string(),
        verification_ref: input.verification_ref.to_string(),
        repair_ref: input.repair_ref.map(str::to_string),
        path_redacted: true,
        content_redacted: true,
        evidence_refs: vec![
            "integration:verification-receipt".to_string(),
            "git:apply-check".to_string(),
        ],
        generated_at_ms: input.generated_at_ms,
    }
}

fn candidate_planned_verifier_count(candidate: &IntegrationCandidateReceipt) -> usize {
    candidate
        .planned_verifier_count
        .max(candidate.planner_required_verifier_count)
        .clamp(1, MAX_INTEGRATION_VERIFIER_LANES)
}

fn empty_verifier_lanes() -> Vec<opensks_contracts::IntegrationVerifierLaneReceipt> {
    Vec::new()
}

fn add_planner_shard_verification_evidence(
    receipt: &mut opensks_contracts::IntegrationVerificationReceipt,
    candidate: &IntegrationCandidateReceipt,
) {
    if candidate.shard_policy_id.is_none() {
        return;
    }
    for evidence in [
        "planner:shard-policy",
        "integration:planner-shard-selection",
    ] {
        if !receipt.evidence_refs.iter().any(|item| item == evidence) {
            receipt.evidence_refs.push(evidence.to_string());
        }
    }
}

fn run_read_only_integration_verifier_lanes(
    workspace: &Path,
    base_envelope: &opensks_contracts::PatchEnvelope,
    unified_diff: &str,
    run_id: &str,
    planned_verifier_count: usize,
    generated_at_ms: u64,
) -> (
    Vec<opensks_contracts::IntegrationVerifierLaneReceipt>,
    Option<(&'static str, bool)>,
) {
    let lane_count = planned_verifier_count.clamp(1, MAX_INTEGRATION_VERIFIER_LANES);
    let mut lanes = Vec::with_capacity(lane_count);
    let mut first_failure = None;
    for lane_index in 0..lane_count {
        let mut envelope = base_envelope.clone();
        envelope.id = format!("{}-lane-{}", base_envelope.id, lane_index + 1);
        envelope.work_item_id = format!("{}-lane-{}", base_envelope.work_item_id, lane_index + 1);
        envelope.lease_id = format!("{}-lane-{}", base_envelope.lease_id, lane_index + 1);
        let result = opensks_git::check_unified_diff_apply(workspace, &envelope, unified_diff);
        let (state, reason_code, passed_gates, failed_gates) = match result {
            Ok(()) => (
                "passed",
                "read_only_git_apply_check_passed",
                vec!["git_apply_check_passed".to_string()],
                Vec::new(),
            ),
            Err(error) => {
                let reason_code = integration_apply_error_reason(&error);
                if first_failure.is_none() {
                    first_failure = Some((reason_code, integration_apply_needs_repair(&error)));
                }
                (
                    "failed",
                    reason_code,
                    Vec::new(),
                    vec!["git_apply_check_passed".to_string()],
                )
            }
        };
        lanes.push(opensks_contracts::IntegrationVerifierLaneReceipt {
            id: format!("integration-verifier-lane-{run_id}-{}", lane_index + 1),
            lane_index,
            worker_id: format!("integration-verifier-lane-{}", lane_index + 1),
            verifier_kind: "read_only_git_apply_check".to_string(),
            state: state.to_string(),
            reason_code: reason_code.to_string(),
            passed_gates,
            failed_gates,
            path_redacted: true,
            content_redacted: true,
            evidence_refs: vec![
                "integration:read-only-verifier-lane".to_string(),
                "git:apply-check".to_string(),
            ],
            generated_at_ms,
        });
    }
    (lanes, first_failure)
}

fn run_semantic_verifier_gate_lanes(
    workspace: &Path,
    run_id: &str,
    lane_index_offset: usize,
    generated_at_ms: u64,
) -> Result<Vec<opensks_contracts::IntegrationVerifierLaneReceipt>, String> {
    let root = workspace
        .join(".opensks")
        .join("runtime")
        .join("semantic-verifiers")
        .join(run_id);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut judgment_paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            judgment_paths.push(entry.path().join("judgment.json"));
        }
    }
    judgment_paths.sort();

    let mut lanes = Vec::with_capacity(judgment_paths.len());
    for (offset, path) in judgment_paths.into_iter().enumerate() {
        let lane_index = lane_index_offset + offset;
        let lane_number = lane_index + 1;
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<SemanticVerifierJudgmentReceipt>(&raw).ok());
        let (worker_id, state, reason_code, mut passed_gates, mut failed_gates) = match parsed {
            Some(receipt)
                if receipt.schema == opensks_contracts::SEMANTIC_VERIFIER_JUDGMENT_SCHEMA
                    && receipt.role == "verification"
                    && receipt.verifier_kind == "model_semantic_judgment" =>
            {
                let verdict = receipt.verdict.trim().to_ascii_lowercase();
                if receipt.state == "judgment_ready"
                    && matches!(verdict.as_str(), "pass" | "passed")
                {
                    (
                        receipt.worker_id,
                        "passed",
                        "semantic_verifier_judgment_passed",
                        receipt.passed_gates,
                        Vec::new(),
                    )
                } else {
                    let reason_code = if verdict.is_empty() || verdict == "unknown" {
                        "semantic_verifier_verdict_missing"
                    } else {
                        "semantic_verifier_rejected"
                    };
                    (
                        receipt.worker_id,
                        "failed",
                        reason_code,
                        Vec::new(),
                        if receipt.failed_gates.is_empty() {
                            vec!["semantic_verifier_verdict_passed".to_string()]
                        } else {
                            receipt.failed_gates
                        },
                    )
                }
            }
            Some(receipt) => (
                receipt.worker_id,
                "failed",
                "semantic_verifier_judgment_invalid",
                Vec::new(),
                vec!["semantic_verifier_judgment_valid".to_string()],
            ),
            None => (
                format!("integration-semantic-verifier-lane-{lane_number}"),
                "failed",
                "semantic_verifier_judgment_invalid",
                Vec::new(),
                vec!["semantic_verifier_judgment_valid".to_string()],
            ),
        };
        if state == "passed" {
            if !passed_gates
                .iter()
                .any(|gate| gate == "semantic_verifier_gate_passed")
            {
                passed_gates.push("semantic_verifier_gate_passed".to_string());
            }
        } else if !failed_gates
            .iter()
            .any(|gate| gate == "semantic_verifier_gate_passed")
        {
            failed_gates.push("semantic_verifier_gate_passed".to_string());
        }
        lanes.push(opensks_contracts::IntegrationVerifierLaneReceipt {
            id: format!("integration-semantic-verifier-lane-{run_id}-{lane_number}"),
            lane_index,
            worker_id,
            verifier_kind: "model_semantic_judgment".to_string(),
            state: state.to_string(),
            reason_code: reason_code.to_string(),
            passed_gates,
            failed_gates,
            path_redacted: true,
            content_redacted: true,
            evidence_refs: vec![
                "integration:semantic-verifier-gate".to_string(),
                "daemon:semantic-verifier-judgment".to_string(),
            ],
            generated_at_ms,
        });
    }
    Ok(lanes)
}

fn semantic_verifier_gate_failure(
    lanes: &[opensks_contracts::IntegrationVerifierLaneReceipt],
) -> Option<&str> {
    lanes
        .iter()
        .find(|lane| lane.state != "passed")
        .map(|lane| lane.reason_code.as_str())
}

fn add_semantic_verifier_gate_evidence(
    receipt: &mut opensks_contracts::IntegrationVerificationReceipt,
    semantic_verifier_lanes_present: bool,
) {
    if !semantic_verifier_lanes_present {
        return;
    }
    if !receipt
        .evidence_refs
        .iter()
        .any(|item| item == "integration:semantic-verifier-gate")
    {
        receipt
            .evidence_refs
            .push("integration:semantic-verifier-gate".to_string());
    }
}

#[allow(clippy::too_many_arguments)]
fn verify_integration_candidate_patch(
    workspace: &Path,
    candidate_dir: &Path,
    run_id: &str,
    candidate_id: &str,
    candidate: &IntegrationCandidateReceipt,
    target_paths: Vec<String>,
    candidate_ref: &str,
    patch_ref: &str,
    verification_ref: &str,
    repair_ref: &str,
    integration_ref: &str,
    now_ms: u64,
) -> Result<(opensks_contracts::IntegrationVerificationReceipt, String), String> {
    let unified_diff = match std::fs::read_to_string(candidate_dir.join("candidate.patch")) {
        Ok(diff) if !diff.trim().is_empty() => diff,
        Ok(_) => {
            let receipt = integration_verification_receipt(IntegrationVerificationReceiptInput {
                run_id,
                candidate_id,
                state: "failed",
                reason_code: "candidate_patch_empty",
                target_paths,
                passed_gates: vec![
                    "candidate_receipt_valid".to_string(),
                    "target_policy_passed".to_string(),
                ],
                failed_gates: vec!["candidate_patch_present".to_string()],
                candidate_ref,
                patch_ref,
                verification_ref,
                repair_ref: None,
                planned_verifier_count: candidate_planned_verifier_count(candidate),
                verifier_lanes: empty_verifier_lanes(),
                generated_at_ms: now_ms,
            });
            write_integration_verification_receipt(candidate_dir, &receipt)?;
            return Ok((receipt, String::new()));
        }
        Err(_) => {
            let receipt = integration_verification_receipt(IntegrationVerificationReceiptInput {
                run_id,
                candidate_id,
                state: "failed",
                reason_code: "candidate_patch_missing",
                target_paths,
                passed_gates: vec![
                    "candidate_receipt_valid".to_string(),
                    "target_policy_passed".to_string(),
                ],
                failed_gates: vec!["candidate_patch_present".to_string()],
                candidate_ref,
                patch_ref,
                verification_ref,
                repair_ref: None,
                planned_verifier_count: candidate_planned_verifier_count(candidate),
                verifier_lanes: empty_verifier_lanes(),
                generated_at_ms: now_ms,
            });
            write_integration_verification_receipt(candidate_dir, &receipt)?;
            return Ok((receipt, String::new()));
        }
    };

    let mut envelope = opensks_git::new_patch_envelope(
        format!("integration-verification-{run_id}"),
        "integration-verifier",
        format!("verification-integration-{run_id}"),
        target_paths.clone(),
    );
    envelope.base_commit = candidate
        .source_base_commit
        .clone()
        .filter(|value| !value.is_empty());
    envelope.unified_diff_ref = patch_ref.to_string();
    envelope.rollback_ref = "memory://integration-verifier-read-only".to_string();

    if let Some(base_commit) = envelope.base_commit.as_deref() {
        let changed_since_base =
            opensks_git::target_paths_changed_since_base(workspace, base_commit, &target_paths)
                .map_err(|error| error.to_string())?;
        if changed_since_base {
            let reason_code = "target_changed_since_candidate_base";
            let item = integration_repair_item(IntegrationRepairItemInput {
                run_id,
                candidate_id,
                reason_code,
                target_paths: target_paths.clone(),
                conflict_paths: target_paths.clone(),
                candidate_ref,
                patch_ref,
                integration_ref,
                repair_ref,
                generated_at_ms: now_ms,
            });
            write_integration_repair_item(candidate_dir, &item)?;
            let mut receipt =
                integration_verification_receipt(IntegrationVerificationReceiptInput {
                    run_id,
                    candidate_id,
                    state: "failed",
                    reason_code,
                    target_paths,
                    passed_gates: vec![
                        "candidate_receipt_valid".to_string(),
                        "target_policy_passed".to_string(),
                        "candidate_patch_present".to_string(),
                    ],
                    failed_gates: vec!["target_base_fence".to_string()],
                    candidate_ref,
                    patch_ref,
                    verification_ref,
                    repair_ref: Some(repair_ref),
                    planned_verifier_count: candidate_planned_verifier_count(candidate),
                    verifier_lanes: empty_verifier_lanes(),
                    generated_at_ms: now_ms,
                });
            add_planner_shard_verification_evidence(&mut receipt, candidate);
            write_integration_verification_receipt(candidate_dir, &receipt)?;
            return Ok((receipt, String::new()));
        }
    }

    let planned_verifier_count = candidate_planned_verifier_count(candidate);
    let (verifier_lanes, verifier_failure) = run_read_only_integration_verifier_lanes(
        workspace,
        &envelope,
        &unified_diff,
        run_id,
        planned_verifier_count,
        now_ms,
    );
    let semantic_verifier_lanes =
        run_semantic_verifier_gate_lanes(workspace, run_id, verifier_lanes.len(), now_ms)?;
    let semantic_verifier_lanes_present = !semantic_verifier_lanes.is_empty();
    let semantic_failure = semantic_verifier_gate_failure(&semantic_verifier_lanes)
        .map(std::string::ToString::to_string);
    let read_only_verifier_failed = verifier_failure.is_some();
    let semantic_verifier_failed = semantic_failure.is_some();
    let total_planned_verifier_count =
        planned_verifier_count.saturating_add(semantic_verifier_lanes.len());
    let mut verifier_lanes = verifier_lanes;
    verifier_lanes.extend(semantic_verifier_lanes);
    let blocking_failure = verifier_failure
        .map(|(reason, needs_repair)| (reason.to_string(), needs_repair))
        .or_else(|| semantic_failure.map(|reason| (reason, false)));
    match blocking_failure {
        None => {
            let mut passed_gates = vec![
                "candidate_receipt_valid".to_string(),
                "target_policy_passed".to_string(),
                "candidate_patch_present".to_string(),
                "target_base_fence".to_string(),
                "main_workspace_clean_for_targets".to_string(),
                "git_apply_check_passed".to_string(),
                "read_only_verifier_lanes_passed".to_string(),
            ];
            if candidate.shard_policy_id.is_some() {
                passed_gates.push("planner_required_shards_present".to_string());
            }
            if semantic_verifier_lanes_present {
                passed_gates.push("semantic_verifier_gate_passed".to_string());
            }
            let mut receipt =
                integration_verification_receipt(IntegrationVerificationReceiptInput {
                    run_id,
                    candidate_id,
                    state: "passed",
                    reason_code: "candidate_verification_passed",
                    target_paths,
                    passed_gates,
                    failed_gates: Vec::new(),
                    candidate_ref,
                    patch_ref,
                    verification_ref,
                    repair_ref: None,
                    planned_verifier_count: total_planned_verifier_count,
                    verifier_lanes,
                    generated_at_ms: now_ms,
                });
            add_planner_shard_verification_evidence(&mut receipt, candidate);
            add_semantic_verifier_gate_evidence(&mut receipt, semantic_verifier_lanes_present);
            write_integration_verification_receipt(candidate_dir, &receipt)?;
            Ok((receipt, unified_diff))
        }
        Some((reason_code, needs_repair)) => {
            let mut failed_gates = Vec::new();
            if read_only_verifier_failed {
                failed_gates.push("git_apply_check_passed".to_string());
                failed_gates.push("read_only_verifier_lanes_passed".to_string());
            }
            if semantic_verifier_failed {
                failed_gates.push("semantic_verifier_gate_passed".to_string());
            }
            let receipt_repair_ref = needs_repair
                .then(|| {
                    let item = integration_repair_item(IntegrationRepairItemInput {
                        run_id,
                        candidate_id,
                        reason_code: &reason_code,
                        target_paths: target_paths.clone(),
                        conflict_paths: target_paths.clone(),
                        candidate_ref,
                        patch_ref,
                        integration_ref,
                        repair_ref,
                        generated_at_ms: now_ms,
                    });
                    write_integration_repair_item(candidate_dir, &item)?;
                    Ok::<_, String>(repair_ref.to_string())
                })
                .transpose()?;
            let mut receipt =
                integration_verification_receipt(IntegrationVerificationReceiptInput {
                    run_id,
                    candidate_id,
                    state: "failed",
                    reason_code: &reason_code,
                    target_paths,
                    passed_gates: vec![
                        "candidate_receipt_valid".to_string(),
                        "target_policy_passed".to_string(),
                        "candidate_patch_present".to_string(),
                    ],
                    failed_gates,
                    candidate_ref,
                    patch_ref,
                    verification_ref,
                    repair_ref: receipt_repair_ref.as_deref(),
                    planned_verifier_count: total_planned_verifier_count,
                    verifier_lanes,
                    generated_at_ms: now_ms,
                });
            add_planner_shard_verification_evidence(&mut receipt, candidate);
            add_semantic_verifier_gate_evidence(&mut receipt, semantic_verifier_lanes_present);
            write_integration_verification_receipt(candidate_dir, &receipt)?;
            Ok((receipt, String::new()))
        }
    }
}

struct IntegrationApplyReceiptInput<'a> {
    run_id: &'a str,
    candidate_id: &'a str,
    state: &'a str,
    reason_code: &'a str,
    target_paths: Vec<String>,
    approval_id: Option<String>,
    approval_policy_id: Option<&'a str>,
    turn_settings: Option<&'a opensks_contracts::IntegrationTurnSettingsSnapshot>,
    candidate_ref: &'a str,
    patch_ref: &'a str,
    verification_ref: Option<&'a str>,
    integration_ref: &'a str,
    final_diff_ref: &'a str,
    seal_ref: Option<&'a str>,
    repair_ref: Option<&'a str>,
    cleanup_ref: Option<&'a str>,
    main_workspace_modified: bool,
    verifier_passed: bool,
    generated_at_ms: u64,
}

fn integration_apply_receipt(
    input: IntegrationApplyReceiptInput<'_>,
) -> opensks_contracts::IntegrationApplyReceipt {
    let mut evidence_refs = vec![
        "integration:coordinator-apply".to_string(),
        "git:apply-check".to_string(),
        "git:atomic-apply".to_string(),
    ];
    if input.main_workspace_modified {
        evidence_refs.push("integration:path-write-lease".to_string());
        evidence_refs.push("scheduler:path-scope-bound".to_string());
        evidence_refs.push("scheduler:path-scope-external-write".to_string());
    }
    if input.verification_ref.is_some() {
        evidence_refs.push("integration:verification-receipt".to_string());
    }
    if input.approval_policy_id.is_some() {
        evidence_refs.push("settings:approval-policy".to_string());
    }
    if input.turn_settings.is_some() {
        evidence_refs.push("settings:turn-settings-snapshot".to_string());
    }
    if input.cleanup_ref.is_some() {
        evidence_refs.push("integration:cleanup-receipt".to_string());
    }
    opensks_contracts::IntegrationApplyReceipt {
        schema: opensks_contracts::INTEGRATION_APPLY_RECEIPT_SCHEMA.to_string(),
        id: format!("integration-apply-{}", input.run_id),
        run_id: input.run_id.to_string(),
        candidate_id: input.candidate_id.to_string(),
        state: input.state.to_string(),
        reason_code: input.reason_code.to_string(),
        target_paths: input.target_paths,
        approval_required: true,
        approval_policy_id: input.approval_policy_id.map(str::to_string),
        turn_settings: input.turn_settings.cloned(),
        approval_id: input.approval_id,
        candidate_ref: input.candidate_ref.to_string(),
        patch_ref: input.patch_ref.to_string(),
        verification_ref: input.verification_ref.map(str::to_string),
        receipt_ref: input.integration_ref.to_string(),
        integration_ref: input.integration_ref.to_string(),
        final_diff_ref: input.final_diff_ref.to_string(),
        seal_ref: input.seal_ref.map(str::to_string),
        repair_ref: input.repair_ref.map(str::to_string),
        cleanup_ref: input.cleanup_ref.map(str::to_string),
        main_workspace_modified: input.main_workspace_modified,
        verifier_passed: input.verifier_passed,
        path_redacted: true,
        content_redacted: true,
        evidence_refs,
        generated_at_ms: input.generated_at_ms,
    }
}

struct IntegrationFinalSealInput<'a> {
    run_id: &'a str,
    candidate_id: &'a str,
    target_paths: Vec<String>,
    approval_id: Option<String>,
    approval_policy_id: Option<&'a str>,
    turn_settings: Option<&'a opensks_contracts::IntegrationTurnSettingsSnapshot>,
    candidate_ref: &'a str,
    patch_ref: &'a str,
    verification_ref: &'a str,
    integration_ref: &'a str,
    final_diff_ref: &'a str,
    seal_ref: &'a str,
    cleanup_ref: Option<&'a str>,
    generated_at_ms: u64,
}

fn integration_final_seal(
    input: IntegrationFinalSealInput<'_>,
) -> opensks_contracts::IntegrationFinalSeal {
    let mut passed_gates = vec![
        "candidate_receipt_valid".to_string(),
        "verification_receipt_passed".to_string(),
        "approval_event_persisted".to_string(),
        "git_apply_check_passed".to_string(),
        "main_workspace_apply_completed".to_string(),
        "final_diff_captured".to_string(),
        "repair_not_required".to_string(),
    ];
    let mut evidence_refs = vec![
        "integration:final-seal".to_string(),
        "integration:verification-receipt".to_string(),
        "integration:approval-gated-apply".to_string(),
        "git:atomic-apply".to_string(),
        "git:final-diff".to_string(),
    ];
    if input.cleanup_ref.is_some() {
        passed_gates.push("source_isolation_cleanup_receipt".to_string());
        evidence_refs.push("integration:cleanup-receipt".to_string());
    }
    if input.approval_policy_id.is_some() {
        passed_gates.push("approval_policy_bound".to_string());
        evidence_refs.push("settings:approval-policy".to_string());
    }
    if input.turn_settings.is_some() {
        passed_gates.push("turn_settings_snapshot_bound".to_string());
        evidence_refs.push("settings:turn-settings-snapshot".to_string());
    }
    opensks_contracts::IntegrationFinalSeal {
        schema: opensks_contracts::INTEGRATION_FINAL_SEAL_SCHEMA.to_string(),
        id: format!("integration-final-seal-{}", input.run_id),
        run_id: input.run_id.to_string(),
        candidate_id: input.candidate_id.to_string(),
        state: "sealed".to_string(),
        reason_code: "integration_final_sealed".to_string(),
        target_paths: input.target_paths,
        passed_gates,
        failed_gates: Vec::new(),
        approval_id: input.approval_id,
        approval_policy_id: input.approval_policy_id.map(str::to_string),
        turn_settings: input.turn_settings.cloned(),
        candidate_ref: input.candidate_ref.to_string(),
        patch_ref: input.patch_ref.to_string(),
        verification_ref: input.verification_ref.to_string(),
        integration_ref: input.integration_ref.to_string(),
        final_diff_ref: input.final_diff_ref.to_string(),
        seal_ref: input.seal_ref.to_string(),
        repair_ref: None,
        cleanup_ref: input.cleanup_ref.map(str::to_string),
        path_redacted: true,
        content_redacted: true,
        evidence_refs,
        generated_at_ms: input.generated_at_ms,
    }
}

struct IntegrationCleanupReceiptInput<'a> {
    run_id: &'a str,
    candidate_id: &'a str,
    candidate: &'a IntegrationCandidateReceipt,
    integration_ref: &'a str,
    seal_ref: &'a str,
    cleanup_ref: &'a str,
    candidate_ref: &'a str,
    patch_ref: &'a str,
    final_diff_ref: &'a str,
    generated_at_ms: u64,
}

fn integration_cleanup_receipt(
    workspace: &Path,
    input: IntegrationCleanupReceiptInput<'_>,
) -> opensks_contracts::IntegrationCleanupReceipt {
    let mut seen_workers = BTreeSet::new();
    let mut source_isolations = Vec::new();
    for source in &input.candidate.source_candidates {
        let Some(source_isolation_id) = source
            .source_isolation_id
            .as_deref()
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if !seen_workers.insert(source.worker_id.clone()) {
            continue;
        }
        source_isolations.push(cleanup_integration_source_isolation(
            workspace,
            input.run_id,
            source_isolation_id.to_string(),
            source
                .source_isolation_mode
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            source.worker_id.clone(),
            source.work_item_id.clone(),
        ));
    }
    if source_isolations.is_empty() {
        if let Some(source_isolation_id) = input
            .candidate
            .source_isolation_id
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let isolation_prefix = format!("isolation-{}-", input.run_id);
            let worker_id = source_isolation_id
                .strip_prefix(&isolation_prefix)
                .unwrap_or(&input.candidate.worker_id)
                .to_string();
            source_isolations.push(cleanup_integration_source_isolation(
                workspace,
                input.run_id,
                source_isolation_id.to_string(),
                input
                    .candidate
                    .source_isolation_mode
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                worker_id,
                None,
            ));
        }
    }
    let cleanup_target_count = source_isolations.len();
    let cleaned_count = source_isolations
        .iter()
        .filter(|target| target.removed || !target.existed)
        .count();
    let (state, reason_code) = if cleanup_target_count == 0 {
        ("not_required", "no_source_isolations_to_clean")
    } else if cleaned_count == cleanup_target_count {
        ("cleaned", "source_isolations_cleaned")
    } else {
        ("partial", "source_isolation_cleanup_partial")
    };
    opensks_contracts::IntegrationCleanupReceipt {
        schema: opensks_contracts::INTEGRATION_CLEANUP_RECEIPT_SCHEMA.to_string(),
        id: format!("integration-cleanup-{}", input.run_id),
        run_id: input.run_id.to_string(),
        candidate_id: input.candidate_id.to_string(),
        state: state.to_string(),
        reason_code: reason_code.to_string(),
        integration_ref: input.integration_ref.to_string(),
        seal_ref: input.seal_ref.to_string(),
        cleanup_ref: input.cleanup_ref.to_string(),
        source_isolations,
        cleanup_target_count,
        cleaned_count,
        retained_candidate_ref: input.candidate_ref.to_string(),
        retained_patch_ref: input.patch_ref.to_string(),
        retained_final_diff_ref: input.final_diff_ref.to_string(),
        path_redacted: true,
        content_redacted: true,
        evidence_refs: vec![
            "integration:cleanup-receipt".to_string(),
            "git:source-isolation-cleanup".to_string(),
        ],
        generated_at_ms: input.generated_at_ms,
    }
}

fn cleanup_integration_source_isolation(
    workspace: &Path,
    run_id: &str,
    source_isolation_id: String,
    source_isolation_mode: String,
    worker_id: String,
    work_item_id: Option<String>,
) -> opensks_contracts::IntegrationCleanupTarget {
    let cleanup_result = opensks_git::cleanup_isolation(workspace, run_id, &worker_id);
    let (existed, removed, reason_code) = match cleanup_result {
        Ok(result) => (result.existed, result.removed, result.reason_code),
        Err(_) => (true, false, "source_isolation_cleanup_failed".to_string()),
    };
    opensks_contracts::IntegrationCleanupTarget {
        source_isolation_id,
        source_isolation_mode,
        worker_id,
        work_item_id,
        existed,
        removed,
        reason_code,
    }
}

struct IntegrationRepairItemInput<'a> {
    run_id: &'a str,
    candidate_id: &'a str,
    reason_code: &'a str,
    target_paths: Vec<String>,
    conflict_paths: Vec<String>,
    candidate_ref: &'a str,
    patch_ref: &'a str,
    integration_ref: &'a str,
    repair_ref: &'a str,
    generated_at_ms: u64,
}

fn integration_repair_item(
    input: IntegrationRepairItemInput<'_>,
) -> opensks_contracts::IntegrationRepairItem {
    opensks_contracts::IntegrationRepairItem {
        schema: opensks_contracts::INTEGRATION_REPAIR_ITEM_SCHEMA.to_string(),
        id: format!("integration-repair-{}", input.run_id),
        run_id: input.run_id.to_string(),
        candidate_id: input.candidate_id.to_string(),
        state: "repair_required".to_string(),
        reason_code: input.reason_code.to_string(),
        target_paths: input.target_paths,
        conflict_paths: input.conflict_paths,
        candidate_ref: input.candidate_ref.to_string(),
        patch_ref: input.patch_ref.to_string(),
        integration_ref: input.integration_ref.to_string(),
        repair_ref: input.repair_ref.to_string(),
        suggested_actions: vec![
            "review main-workspace changes for the conflicted paths".to_string(),
            "regenerate or rebase the isolated candidate against the current workspace".to_string(),
            "rerun targeted verification before approval-gated apply".to_string(),
        ],
        path_redacted: true,
        content_redacted: true,
        evidence_refs: vec![
            "integration:repair-required".to_string(),
            "git:atomic-apply".to_string(),
        ],
        generated_at_ms: input.generated_at_ms,
    }
}

fn write_integration_repair_item(
    candidate_dir: &Path,
    item: &opensks_contracts::IntegrationRepairItem,
) -> Result<(), String> {
    std::fs::write(
        candidate_dir.join("repair.json"),
        serde_json::to_string_pretty(item).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn write_integration_verification_receipt(
    candidate_dir: &Path,
    receipt: &opensks_contracts::IntegrationVerificationReceipt,
) -> Result<(), String> {
    std::fs::write(
        candidate_dir.join("verification.json"),
        serde_json::to_string_pretty(receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn write_integration_apply_receipt(
    candidate_dir: &Path,
    receipt: &opensks_contracts::IntegrationApplyReceipt,
) -> Result<(), String> {
    std::fs::write(
        candidate_dir.join("integration.json"),
        serde_json::to_string_pretty(receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn write_integration_final_seal(
    candidate_dir: &Path,
    seal: &opensks_contracts::IntegrationFinalSeal,
) -> Result<(), String> {
    std::fs::write(
        candidate_dir.join("seal.json"),
        serde_json::to_string_pretty(seal).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn write_integration_cleanup_receipt(
    candidate_dir: &Path,
    receipt: &opensks_contracts::IntegrationCleanupReceipt,
) -> Result<(), String> {
    std::fs::write(
        candidate_dir.join("cleanup.json"),
        serde_json::to_string_pretty(receipt).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn append_integration_apply_event(
    workspace: &Path,
    receipt: &opensks_contracts::IntegrationApplyReceipt,
) -> Result<(), String> {
    let kind = match receipt.state.as_str() {
        "integrated" => opensks_contracts::EventKind::WorkItemCompleted,
        "awaiting_approval" => opensks_contracts::EventKind::ApprovalRequested,
        _ => opensks_contracts::EventKind::VerificationFailed,
    };
    let now_ms = timestamp_ms();
    let path_write_lease = receipt
        .main_workspace_modified
        .then(|| integration_apply_path_write_lease(receipt, now_ms));
    let path_scope = receipt
        .main_workspace_modified
        .then(|| integration_apply_path_scope(&receipt.target_paths));
    let event = opensks_contracts::ExecutionEventEnvelope {
        schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("integration-apply-{}-{}", receipt.run_id, receipt.state),
        run_id: receipt.run_id.clone(),
        sequence: 0,
        occurred_at: event_time_from_ms(now_ms),
        actor: "integration-coordinator".to_string(),
        causation_id: Some(receipt.candidate_id.clone()),
        correlation_id: Some(receipt.id.clone()),
        kind,
        payload: serde_json::json!({
            "source": "integration.candidate.apply",
            "receipt_schema": receipt.schema,
            "candidate_id": receipt.candidate_id,
            "state": receipt.state,
            "reason_code": receipt.reason_code,
            "target_count": receipt.target_paths.len(),
            "approval_required": receipt.approval_required,
            "approval_id": receipt.approval_id,
            "candidate_ref": receipt.candidate_ref,
            "patch_ref": receipt.patch_ref,
            "verification_ref": receipt.verification_ref,
            "integration_ref": receipt.integration_ref,
            "final_diff_ref": receipt.final_diff_ref,
            "seal_ref": receipt.seal_ref,
            "repair_ref": receipt.repair_ref,
            "cleanup_ref": receipt.cleanup_ref,
            "main_workspace_modified": receipt.main_workspace_modified,
            "verifier_passed": receipt.verifier_passed,
            "path_write_lease": path_write_lease,
            "path_scope": path_scope,
            "path_redacted": true,
            "content_redacted": true
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: receipt.evidence_refs.clone(),
    };
    let mut store = opensks_event_store::EventStore::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    store
        .append_event(event)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn integration_apply_path_scope(target_paths: &[String]) -> opensks_contracts::PathScope {
    opensks_contracts::PathScope {
        workspace_relative_roots: target_paths.to_vec(),
        allow_external_write: true,
    }
}

fn integration_apply_path_write_lease(
    receipt: &opensks_contracts::IntegrationApplyReceipt,
    acquired_at_ms: u64,
) -> opensks_contracts::Lease {
    opensks_contracts::Lease {
        id: format!("path-write-{}", receipt.run_id),
        lease_type: opensks_contracts::LeaseType::PathWrite,
        holder: "integration-coordinator".to_string(),
        acquired_at_ms,
        last_heartbeat_at_ms: Some(acquired_at_ms),
        ttl_ms: opensks_scheduler::DEFAULT_WORKER_LEASE_TTL_MS,
    }
}

fn integration_apply_needs_repair(error: &opensks_git::GitError) -> bool {
    matches!(
        error,
        opensks_git::GitError::DirtyPath(_)
            | opensks_git::GitError::BeforeHashMismatch { .. }
            | opensks_git::GitError::VerificationFailedRolledBack
            | opensks_git::GitError::GitCommand(_)
    )
}

fn invalid_integration_target(path: &str) -> bool {
    let relative = Path::new(path);
    relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        || looks_secret_integration_target(path)
}

fn looks_secret_integration_target(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".env")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower.contains("id_rsa")
        || lower.contains(".pem")
}

fn integration_apply_error_reason(error: &opensks_git::GitError) -> &'static str {
    match error {
        opensks_git::GitError::DirtyPath(_) => "target_dirty",
        opensks_git::GitError::BeforeHashMismatch { .. } => "before_hash_mismatch",
        opensks_git::GitError::VerificationFailedRolledBack => "verifier_failed_rolled_back",
        opensks_git::GitError::GitRequired => "git_repository_required",
        opensks_git::GitError::PathEscape(_) => "candidate_target_rejected",
        opensks_git::GitError::GitCommand(_) => "git_apply_failed",
        opensks_git::GitError::SecretStageBlocked(_) => "candidate_target_rejected",
        opensks_git::GitError::ProtectedBranch(_) => "approval_required",
        opensks_git::GitError::DuplicateOutboxWrite(_) => "duplicate_integration_apply",
        opensks_git::GitError::EmptyOutbox => "integration_outbox_empty",
        opensks_git::GitError::Io(_) => "integration_io_error",
        opensks_git::GitError::Json(_) => "integration_json_error",
    }
}

fn artifact_ref(relative: &Path) -> String {
    format!(
        "artifact://{}",
        relative.to_string_lossy().replace('\\', "/")
    )
}

#[derive(Debug, Clone)]
struct TurnExecutionWorkspace {
    path: PathBuf,
    isolation: Option<opensks_contracts::GitIsolationReport>,
}

impl TurnExecutionWorkspace {
    fn mode_label(&self) -> String {
        self.isolation
            .as_ref()
            .map(|report| isolation_mode_label(&report.mode))
            .unwrap_or_else(|| "direct_workspace".to_string())
    }

    fn reason_code(&self) -> String {
        self.isolation
            .as_ref()
            .map(|report| report.reason_code.clone())
            .unwrap_or_else(|| "direct_workspace_execution".to_string())
    }
}

fn prepare_turn_execution_workspace(
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
    should_isolate_execution: bool,
) -> Result<TurnExecutionWorkspace, opensks_git::GitError> {
    if should_isolate_execution {
        let report = opensks_git::create_isolation(workspace, &lease.run_id, "turn-supervisor")?;
        let path = PathBuf::from(&report.worktree_path);
        return Ok(TurnExecutionWorkspace {
            path,
            isolation: Some(report),
        });
    }
    Ok(TurnExecutionWorkspace {
        path: workspace.to_path_buf(),
        isolation: None,
    })
}

fn resolve_claimed_turn_routing(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
    settings: &opensks_contracts::ConversationTurnSettings,
    now_ms: u64,
) -> Result<opensks_contracts::RoutingDecision, DaemonError> {
    let provider_repo = opensks_provider::ProviderRepository::open_workspace(workspace)
        .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
    let decision = opensks_provider::resolve_routing_decision_from_repository(
        &provider_repo,
        format!("route-{}", lease.turn_id),
        settings,
    )
    .map_err(|error| DaemonError::Io(std::io::Error::other(error.to_string())))?;
    let decision_json = serde_json::to_string(&decision)?;
    repo.set_turn_model_routing_decision(&lease.turn_id, &decision_json, now_ms)?;
    Ok(decision)
}

struct ProviderDispatchTarget {
    connection: opensks_contracts::ProviderConnection,
    model: opensks_contracts::ModelCatalogEntry,
    bearer_token: String,
    routing_decision: opensks_contracts::RoutingDecision,
}

struct ProviderImageToolExecutor {
    registry: opensks_provider::ModelRegistry,
    connections: HashMap<String, opensks_contracts::ProviderConnection>,
    image_model_id: Option<String>,
    can_generate: bool,
    can_inspect: bool,
}

impl ProviderImageToolExecutor {
    fn available_tool_names(&self) -> Vec<&'static str> {
        let mut tools = Vec::new();
        if self.can_generate {
            tools.push("image.generate");
        }
        if self.can_inspect {
            tools.push("image.inspect");
        }
        tools
    }
}

impl opensks_adapter::ImageToolExecutor for ProviderImageToolExecutor {
    fn generate_image(
        &self,
        workspace: &Path,
        request: &opensks_adapter::ImageGenerateToolRequest,
    ) -> Result<opensks_contracts::ImageAsset, String> {
        let asset_id = request.asset_id.clone().unwrap_or_else(|| {
            generated_image_asset_id(&request.prompt, request.width, request.height)
        });
        let mut runtime = opensks_image::ImageRuntime::new();
        let asset = runtime
            .generate_provider_asset_file_for_model(
                &self.registry,
                workspace,
                opensks_image::ImageAssetRequest {
                    id: &asset_id,
                    width: request.width,
                    height: request.height,
                    anchors: Vec::new(),
                    prompt: Some(&request.prompt),
                },
                self.image_model_id.as_deref(),
                self,
            )
            .map_err(|error| error.to_string())?;
        persist_image_ledger(workspace, runtime.ledger()).map_err(|error| error.to_string())?;
        Ok(asset)
    }

    fn inspect_image(
        &self,
        workspace: &Path,
        request: &opensks_adapter::ImageInspectToolRequest,
    ) -> Result<opensks_adapter::ImageInspectToolResult, String> {
        let ledger = read_image_ledger(workspace).map_err(|error| error.to_string())?;
        let mut runtime = opensks_image::ImageRuntime::from_ledger(ledger);
        let result = runtime
            .inspect_provider_asset_file(
                &self.registry,
                workspace,
                &request.artifact_ref,
                request.prompt.as_deref(),
                self,
            )
            .map_err(|error| error.to_string())?;
        persist_image_ledger(workspace, runtime.ledger()).map_err(|error| error.to_string())?;
        Ok(opensks_adapter::ImageInspectToolResult {
            receipt: result.receipt,
            text: result.text,
        })
    }
}

impl opensks_image::ImageGenerationClient for ProviderImageToolExecutor {
    fn generate_image(
        &self,
        request: &opensks_image::ImageProviderRequest<'_>,
    ) -> Result<opensks_image::ImageProviderOutput, opensks_image::ImageError> {
        let connection = self.connections.get(request.provider_id).ok_or_else(|| {
            opensks_image::ImageError::Provider("provider connection not found".to_string())
        })?;
        if !supports_openai_compatible_dispatch(connection.kind) {
            return Err(opensks_image::ImageError::Provider(format!(
                "provider kind `{:?}` is not supported by image dispatch",
                connection.kind
            )));
        }
        let bearer_token = resolve_provider_secret_for_dispatch(&connection.auth)
            .map_err(opensks_image::ImageError::Provider)?;
        let generator = opensks_adapter::OpenAiCompatibleImageGenerator::new(
            connection.endpoint.base_url.clone(),
            bearer_token,
        )
        .map_err(|error| opensks_image::ImageError::Provider(error.to_string()))?;
        opensks_image::ImageGenerationClient::generate_image(&generator, request)
    }
}

impl opensks_image::ImageInspectionClient for ProviderImageToolExecutor {
    fn inspect_image(
        &self,
        request: &opensks_image::ImageInspectionProviderRequest<'_>,
    ) -> Result<opensks_image::ImageInspectionProviderOutput, opensks_image::ImageError> {
        let connection = self.connections.get(request.provider_id).ok_or_else(|| {
            opensks_image::ImageError::Provider("provider connection not found".to_string())
        })?;
        if !supports_openai_compatible_dispatch(connection.kind) {
            return Err(opensks_image::ImageError::Provider(format!(
                "provider kind `{:?}` is not supported by vision dispatch",
                connection.kind
            )));
        }
        let bearer_token = resolve_provider_secret_for_dispatch(&connection.auth)
            .map_err(opensks_image::ImageError::Provider)?;
        let generator = opensks_adapter::OpenAiCompatibleImageGenerator::new(
            connection.endpoint.base_url.clone(),
            bearer_token,
        )
        .map_err(|error| opensks_image::ImageError::Provider(error.to_string()))?;
        opensks_image::ImageInspectionClient::inspect_image(&generator, request)
    }
}

fn prepare_image_tool_executor(
    workspace: &Path,
    settings: Option<&opensks_contracts::ConversationTurnSettings>,
) -> Result<ProviderImageToolExecutor, String> {
    let provider_repo = opensks_provider::ProviderRepository::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    let registry = opensks_provider::model_registry_from_repository(&provider_repo)
        .map_err(|error| error.to_string())?;
    let image_model_id = settings
        .and_then(|settings| settings.image_model_id.as_deref())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let mut image_request =
        opensks_provider::RoutingRequest::for_image("daemon-image-generate-tool");
    image_request.explicit_model_id = image_model_id.clone();
    let can_generate =
        preflight_provider_image_tool_route(&registry, &provider_repo, image_request, "image")?;
    let can_inspect = preflight_provider_image_tool_route(
        &registry,
        &provider_repo,
        opensks_provider::RoutingRequest::for_vision("daemon-image-inspect-tool"),
        "vision",
    )?;
    if !can_generate && !can_inspect {
        return Err("no enabled compatible image or vision model".to_string());
    }
    let mut connections = HashMap::new();
    for connection in provider_repo
        .list_connections()
        .map_err(|error| error.to_string())?
    {
        connections.insert(connection.id.clone(), connection);
    }
    Ok(ProviderImageToolExecutor {
        registry,
        connections,
        image_model_id,
        can_generate,
        can_inspect,
    })
}

fn preflight_provider_image_tool_route(
    registry: &opensks_provider::ModelRegistry,
    provider_repo: &opensks_provider::ProviderRepository,
    request: opensks_provider::RoutingRequest,
    label: &str,
) -> Result<bool, String> {
    let decision = registry.route(&request);
    if !decision.status.has_resolved_model() {
        return Ok(false);
    }
    let route_receipt = decision
        .route_receipt
        .as_ref()
        .ok_or_else(|| format!("{label} route has no receipt"))?;
    let provider_id = route_receipt
        .provider_id
        .as_deref()
        .ok_or_else(|| format!("{label} route has no provider id"))?;
    let selected_connection = provider_repo
        .get_connection(provider_id)
        .map_err(|error| error.to_string())?;
    if !supports_openai_compatible_dispatch(selected_connection.kind) {
        return Err(format!(
            "provider kind `{:?}` is not supported by {label} dispatch",
            selected_connection.kind
        ));
    }
    resolve_provider_secret_for_dispatch(&selected_connection.auth)?;
    Ok(true)
}

fn allow_image_tools(config: &mut opensks_adapter::AgenticConfig, tool_names: &[&str]) {
    for tool_name in tool_names {
        if let Some(entry) = config
            .tool_policy
            .entries
            .iter_mut()
            .find(|entry| entry.tool == *tool_name)
        {
            entry.permission = opensks_contracts::ToolPermission::Allow;
        } else {
            config
                .tool_policy
                .entries
                .push(opensks_contracts::ToolPolicyEntry {
                    tool: (*tool_name).to_string(),
                    permission: opensks_contracts::ToolPermission::Allow,
                });
        }
    }
}

fn generated_image_asset_id(prompt: &str, width: u32, height: u32) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in format!("{prompt}:{width}x{height}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("image-{hash:016x}")
}

fn persist_image_ledger(
    workspace: &Path,
    ledger: &opensks_contracts::ImageLedger,
) -> std::io::Result<()> {
    let dir = workspace.join(".opensks").join("assets").join("candidates");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("image-ledger.json");
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(ledger).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, [&body[..], b"\n"].concat())?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn read_image_ledger(workspace: &Path) -> std::io::Result<opensks_contracts::ImageLedger> {
    let path = workspace
        .join(".opensks")
        .join("assets")
        .join("candidates")
        .join("image-ledger.json");
    if !path.exists() {
        return Ok(opensks_contracts::ImageLedger {
            schema: opensks_contracts::IMAGE_LEDGER_SCHEMA.to_string(),
            assets: Vec::new(),
            provenance_receipts: Vec::new(),
            gc_candidate_ids: Vec::new(),
        });
    }
    let body = std::fs::read_to_string(path)?;
    serde_json::from_str(&body).map_err(std::io::Error::other)
}

fn prepare_provider_dispatch(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    lease: &opensks_conversation::TurnSupervisorLease,
    routing_decision: &opensks_contracts::RoutingDecision,
    now_ms: u64,
) -> Result<ProviderDispatchTarget, String> {
    let route_receipt = routing_decision
        .route_receipt
        .as_ref()
        .ok_or_else(|| "resolved route has no receipt".to_string())?;
    let provider_id = route_receipt
        .provider_id
        .as_deref()
        .ok_or_else(|| "resolved route has no provider id".to_string())?;
    let model_id = routing_decision
        .selected_model_id
        .as_deref()
        .ok_or_else(|| "resolved route has no selected model id".to_string())?;
    let provider_repo = opensks_provider::ProviderRepository::open_workspace(workspace)
        .map_err(|error| error.to_string())?;
    let connection = provider_repo
        .get_connection(provider_id)
        .map_err(|error| error.to_string())?;
    if !supports_openai_compatible_dispatch(connection.kind) {
        return Err(format!(
            "provider kind `{:?}` is not supported by chat dispatch",
            connection.kind
        ));
    }
    let model = provider_repo
        .get_model(model_id)
        .map_err(|error| error.to_string())?;
    if model.provider_id != connection.id {
        return Err(format!(
            "model `{}` belongs to `{}` not `{}`",
            model.id, model.provider_id, connection.id
        ));
    }
    let bearer_token = resolve_provider_secret_for_dispatch(&connection.auth)?;
    let routing_decision = persist_routing_status(
        repo,
        &lease.turn_id,
        routing_decision.clone(),
        opensks_contracts::RoutingStatus::DispatchReady,
        "provider_dispatch_ready",
        now_ms,
    )
    .map_err(|error| error.to_string())?;
    Ok(ProviderDispatchTarget {
        connection,
        model,
        bearer_token,
        routing_decision,
    })
}

fn supports_openai_compatible_dispatch(kind: opensks_contracts::ProviderKind) -> bool {
    matches!(
        kind,
        opensks_contracts::ProviderKind::OpenRouter
            | opensks_contracts::ProviderKind::OpenAi
            | opensks_contracts::ProviderKind::CodexLb
            | opensks_contracts::ProviderKind::OpenAiCompatible
            | opensks_contracts::ProviderKind::LocalOpenAiCompatible
    )
}

fn chat_reasoning_effort_wire_for_provider(
    kind: opensks_contracts::ProviderKind,
) -> Option<opensks_adapter::ChatReasoningEffortWire> {
    match kind {
        opensks_contracts::ProviderKind::OpenRouter => {
            Some(opensks_adapter::ChatReasoningEffortWire::OpenRouterReasoningObject)
        }
        opensks_contracts::ProviderKind::OpenAi => {
            Some(opensks_adapter::ChatReasoningEffortWire::OpenAiReasoningEffort)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct ProviderReasoningEffort {
    wire: Option<opensks_adapter::ChatReasoningEffortWire>,
    effort: opensks_contracts::ReasoningEffort,
}

fn provider_reasoning_effort_for_provider(
    kind: opensks_contracts::ProviderKind,
    effort: opensks_contracts::ReasoningEffort,
) -> ProviderReasoningEffort {
    ProviderReasoningEffort {
        wire: chat_reasoning_effort_wire_for_provider(kind),
        effort,
    }
}

fn add_provider_reasoning_effort(
    body: &mut serde_json::Value,
    reasoning_effort: ProviderReasoningEffort,
) {
    let Some(wire) = reasoning_effort.wire else {
        return;
    };
    match wire {
        opensks_adapter::ChatReasoningEffortWire::OpenRouterReasoningObject => {
            body["reasoning"] = serde_json::json!({
                "effort": opensks_adapter::openrouter_reasoning_effort_value(
                    reasoning_effort.effort
                ),
            });
        }
        opensks_adapter::ChatReasoningEffortWire::OpenAiReasoningEffort => {
            body["reasoning_effort"] = serde_json::Value::String(
                opensks_adapter::openai_reasoning_effort_value(reasoning_effort.effort).to_string(),
            );
        }
    }
}

fn persist_routing_status(
    repo: &opensks_conversation::ConversationRepository,
    turn_id: &str,
    mut decision: opensks_contracts::RoutingDecision,
    status: opensks_contracts::RoutingStatus,
    reason_code: &str,
    now_ms: u64,
) -> Result<opensks_contracts::RoutingDecision, DaemonError> {
    decision.status = status;
    decision.reason_code = reason_code.to_string();
    if let Some(route_receipt) = decision.route_receipt.as_mut() {
        route_receipt.reason_code = reason_code.to_string();
    }
    let decision_json = serde_json::to_string(&decision)?;
    repo.set_turn_model_routing_decision(turn_id, &decision_json, now_ms)?;
    Ok(decision)
}

fn max_output_tokens_for_model(model: &opensks_contracts::ModelCatalogEntry) -> u32 {
    model
        .limits
        .max_output_tokens
        .unwrap_or(1024)
        .clamp(1, u32::MAX as u64) as u32
}

fn resolve_provider_secret_for_dispatch(
    auth: &opensks_contracts::SecretRef,
) -> Result<String, String> {
    match auth.store {
        opensks_contracts::SecretStoreKind::MacosKeychain => {
            resolve_macos_keychain_secret_for_dispatch(auth)
        }
        opensks_contracts::SecretStoreKind::TestMemory if cfg!(test) => {
            Ok("fixture-credential".to_string())
        }
        other => Err(format!(
            "provider secret store `{other:?}` is not supported by daemon dispatch"
        )),
    }
}

fn resolve_macos_keychain_secret_for_dispatch(
    auth: &opensks_contracts::SecretRef,
) -> Result<String, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = auth;
        Err("macOS Keychain provider dispatch is only available on macOS".to_string())
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("security")
            .arg("find-generic-password")
            .arg("-s")
            .arg(&auth.service)
            .arg("-a")
            .arg(&auth.account)
            .arg("-w")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|error| format!("keychain lookup failed: {error}"))?;
        if !output.status.success() {
            return Err("provider credential was not found in Keychain".to_string());
        }
        let value = String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        if value.trim().is_empty() {
            return Err("provider credential resolved empty".to_string());
        }
        Ok(value)
    }
}

struct DispatchRecordingCompleter<C> {
    inner: C,
    workspace: PathBuf,
    turn_id: String,
    pending_decision: std::sync::Mutex<Option<opensks_contracts::RoutingDecision>>,
}

impl<C> DispatchRecordingCompleter<C> {
    fn new(
        inner: C,
        workspace: PathBuf,
        turn_id: String,
        routing_decision: opensks_contracts::RoutingDecision,
    ) -> Self {
        Self {
            inner,
            workspace,
            turn_id,
            pending_decision: std::sync::Mutex::new(Some(routing_decision)),
        }
    }

    fn record_dispatched_once(&self) -> Result<(), opensks_adapter::AgentAdapterError> {
        let Some(decision) = self
            .pending_decision
            .lock()
            .map_err(|_| {
                opensks_adapter::AgentAdapterError::Provider(
                    "provider dispatch state lock poisoned".to_string(),
                )
            })?
            .take()
        else {
            return Ok(());
        };
        let repo = opensks_conversation::ConversationRepository::open_workspace(&self.workspace)
            .map_err(|error| opensks_adapter::AgentAdapterError::Provider(error.to_string()))?;
        persist_routing_status(
            &repo,
            &self.turn_id,
            decision,
            opensks_contracts::RoutingStatus::Dispatched,
            "provider_request_dispatched",
            timestamp_ms(),
        )
        .map_err(|error| opensks_adapter::AgentAdapterError::Provider(error.to_string()))?;
        Ok(())
    }
}

impl<C: opensks_adapter::ChatCompleter> opensks_adapter::ChatCompleter
    for DispatchRecordingCompleter<C>
{
    fn complete(
        &self,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, opensks_adapter::AgentAdapterError> {
        self.record_dispatched_once()?;
        self.inner.complete(body)
    }
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
            sequence: 0,
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

    fn emit_run_completed(
        &self,
        request: &opensks_adapter::AgentRunRequest,
        selected_model_id: Option<&str>,
        candidate: Option<&IntegrationCandidateReceipt>,
        now_ms: u64,
    ) -> Result<(), DaemonError> {
        let event = opensks_contracts::ExecutionEventEnvelope {
            schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!("agent-{}-run-completed", request.run_id),
            run_id: request.run_id.clone(),
            sequence: 0,
            occurred_at: event_time_from_ms(now_ms),
            actor: "turn-supervisor".to_string(),
            causation_id: Some(request.turn_id.clone()),
            correlation_id: Some(request.stream_id.clone()),
            kind: opensks_contracts::EventKind::RunCompleted,
            payload: serde_json::json!({
                "source": "conversation.supervisor.tick",
                "project_id": request.project_id,
                "conversation_id": request.conversation_id,
                "turn_id": request.turn_id,
                "stream_id": request.stream_id,
                "selected_model_id": selected_model_id,
                "integration_candidate_ref": candidate.map(|candidate| candidate.receipt_ref.as_str()),
                "integration_patch_ref": candidate.map(|candidate| candidate.patch_ref.as_str()),
                "integration_selection_ref": candidate.and_then(|candidate| candidate.selection_ref.as_deref()),
                "integration_planned_verifier_count": candidate.map(|candidate| candidate.planned_verifier_count),
                "state": "completed"
            }),
            sensitivity: opensks_contracts::Sensitivity::Internal,
            evidence_refs: vec![
                "conversation:turn-supervisor".to_string(),
                "conversation:run-completed".to_string(),
            ],
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

    fn emit_run_failed(
        &self,
        request: &opensks_adapter::AgentRunRequest,
        now_ms: u64,
    ) -> Result<(), DaemonError> {
        let event = opensks_contracts::ExecutionEventEnvelope {
            schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!("agent-{}-run-failed", request.run_id),
            run_id: request.run_id.clone(),
            sequence: 0,
            occurred_at: event_time_from_ms(now_ms),
            actor: "turn-supervisor".to_string(),
            causation_id: Some(request.turn_id.clone()),
            correlation_id: Some(request.stream_id.clone()),
            kind: opensks_contracts::EventKind::VerificationFailed,
            payload: serde_json::json!({
                "source": "conversation.supervisor.tick",
                "project_id": request.project_id,
                "conversation_id": request.conversation_id,
                "turn_id": request.turn_id,
                "stream_id": request.stream_id,
                "state": "failed",
                "reason_code": "turn_supervisor_failed",
                "message": "TurnSupervisor failed."
            }),
            sensitivity: opensks_contracts::Sensitivity::Internal,
            evidence_refs: vec![
                "conversation:turn-supervisor".to_string(),
                "conversation:run-failed".to_string(),
            ],
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
    let actor = event
        .worker_id
        .clone()
        .unwrap_or_else(|| "agent".to_string());
    opensks_contracts::ExecutionEventEnvelope {
        schema: opensks_contracts::EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!(
            "agent-{}-{}-s{}",
            event.run_id,
            sanitize_event_id_component(&actor),
            event.sequence + 2
        ),
        run_id: event.run_id.clone(),
        sequence: 0,
        occurred_at: event_time_from_ms(event.occurred_at_ms),
        actor,
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

fn sanitize_event_id_component(raw: &str) -> String {
    let mut component = raw
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '-',
        })
        .take(96)
        .collect::<String>();
    if component.is_empty() {
        component.push_str("agent");
    }
    component
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
        AgentEventKind::ImageArtifactCreated => opensks_contracts::EventKind::ImageArtifactCreated,
        AgentEventKind::PlanUpdated
        | AgentEventKind::AssistantTextDelta
        | AgentEventKind::ToolCallStarted
        | AgentEventKind::ToolCallOutput
        | AgentEventKind::FilePatchProposed
        | AgentEventKind::VerificationStarted
        | AgentEventKind::WorkerSpawned
        | AgentEventKind::WorkerProgress
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

#[derive(Debug, Clone)]
struct ExecutionPromptResolution {
    prompt: String,
    raw_prompt_restored: bool,
    raw_ciphertext_bytes: usize,
}

fn resolve_execution_prompt(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    turn_id: &str,
    stored_prompt: &str,
) -> Result<ExecutionPromptResolution, String> {
    if !prompt_requires_raw_content(stored_prompt) {
        return Ok(ExecutionPromptResolution {
            prompt: stored_prompt.to_string(),
            raw_prompt_restored: false,
            raw_ciphertext_bytes: 0,
        });
    }
    let Some(raw_content) = repo
        .turn_user_message_raw_content_ciphertext(turn_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(ExecutionPromptResolution {
            prompt: stored_prompt.to_string(),
            raw_prompt_restored: false,
            raw_ciphertext_bytes: 0,
        });
    };
    let identity_text = match resolve_raw_prompt_vault_identity_text(workspace) {
        Ok(identity_text) => identity_text,
        Err(_) => {
            return Ok(ExecutionPromptResolution {
                prompt: stored_prompt.to_string(),
                raw_prompt_restored: false,
                raw_ciphertext_bytes: raw_content.ciphertext.len(),
            });
        }
    };
    let plaintext = match opensks_vault::decrypt_bytes_with_identity_text(
        &raw_content.ciphertext,
        &identity_text,
    ) {
        Ok(plaintext) => plaintext,
        Err(_) => {
            return Ok(ExecutionPromptResolution {
                prompt: stored_prompt.to_string(),
                raw_prompt_restored: false,
                raw_ciphertext_bytes: raw_content.ciphertext.len(),
            });
        }
    };
    let prompt =
        String::from_utf8(plaintext).map_err(|_| "raw prompt was not UTF-8".to_string())?;
    Ok(ExecutionPromptResolution {
        prompt,
        raw_prompt_restored: true,
        raw_ciphertext_bytes: raw_content.ciphertext.len(),
    })
}

fn resolve_raw_prompt_vault_identity_text(workspace: &Path) -> Result<String, String> {
    #[cfg(test)]
    if let Some(identity_text) = test_raw_prompt_identity_text(workspace) {
        return Ok(identity_text);
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = workspace;
        Err("raw prompt vault identity requires macOS Keychain".to_string())
    }
    #[cfg(target_os = "macos")]
    {
        let account = raw_prompt_vault_keychain_account(workspace);
        let identity_bytes = security_framework::passwords::get_generic_password(
            RAW_PROMPT_VAULT_KEYCHAIN_SERVICE,
            &account,
        )
        .map_err(|_| "raw prompt vault identity was not found in Keychain".to_string())?;
        let identity_text = String::from_utf8(identity_bytes)
            .map_err(|_| "raw prompt vault identity was not UTF-8".to_string())?;
        if identity_text.trim().is_empty() {
            return Err("raw prompt vault identity resolved empty".to_string());
        }
        Ok(identity_text)
    }
}

fn provision_raw_prompt_vault_identity_text(workspace: &Path) -> Result<String, String> {
    let identity_text = opensks_vault::generate_identity_text();
    #[cfg(test)]
    {
        if test_raw_prompt_identity_provisioning_disabled(workspace) {
            return Err("raw prompt vault identity provisioning disabled for test".to_string());
        }
        install_test_raw_prompt_identity_text(workspace, identity_text.clone());
        Ok(identity_text)
    }

    #[cfg(all(not(test), not(target_os = "macos")))]
    {
        let _ = workspace;
        let _ = identity_text;
        Err("raw prompt vault identity provisioning requires macOS Keychain".to_string())
    }
    #[cfg(all(not(test), target_os = "macos"))]
    {
        let account = raw_prompt_vault_keychain_account(workspace);
        security_framework::passwords::set_generic_password(
            RAW_PROMPT_VAULT_KEYCHAIN_SERVICE,
            &account,
            identity_text.as_bytes(),
        )
        .map_err(|error| format!("raw prompt vault keychain provision failed: {error}"))?;
        Ok(identity_text)
    }
}

fn raw_prompt_vault_keychain_account(workspace: &Path) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in workspace.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("workspace:{hash:016x}")
}

#[cfg(test)]
static TEST_RAW_PROMPT_IDENTITIES: std::sync::LazyLock<std::sync::Mutex<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(test)]
static TEST_RAW_PROMPT_PROVISIONING_DISABLED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

#[cfg(test)]
fn install_test_raw_prompt_identity_text(workspace: &Path, identity_text: String) {
    TEST_RAW_PROMPT_IDENTITIES
        .lock()
        .expect("raw prompt identity test lock")
        .insert(raw_prompt_vault_keychain_account(workspace), identity_text);
}

#[cfg(test)]
fn disable_test_raw_prompt_identity_provisioning(workspace: &Path) {
    TEST_RAW_PROMPT_PROVISIONING_DISABLED
        .lock()
        .expect("raw prompt identity provisioning test lock")
        .insert(raw_prompt_vault_keychain_account(workspace));
}

#[cfg(test)]
fn test_raw_prompt_identity_text(workspace: &Path) -> Option<String> {
    TEST_RAW_PROMPT_IDENTITIES
        .lock()
        .expect("raw prompt identity test lock")
        .get(&raw_prompt_vault_keychain_account(workspace))
        .cloned()
}

#[cfg(test)]
fn test_raw_prompt_identity_provisioning_disabled(workspace: &Path) -> bool {
    TEST_RAW_PROMPT_PROVISIONING_DISABLED
        .lock()
        .expect("raw prompt identity provisioning test lock")
        .contains(&raw_prompt_vault_keychain_account(workspace))
}

fn prompt_requires_raw_content(prompt: &str) -> bool {
    prompt.contains("[REDACTED]")
}

fn raw_prompt_restored_after_redaction_agent_event(
    request: &opensks_adapter::AgentRunRequest,
    ciphertext_bytes: usize,
    plaintext_bytes: usize,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("prompt-vault-policy".to_string()),
        node_id: None,
        sequence: 0,
        occurred_at_ms: request.now_ms,
        kind: opensks_contracts::AgentEventKind::Warning,
        payload: serde_json::json!({
            "code": "raw_prompt_restored_after_redaction",
            "message": "Raw prompt content was restored from the encrypted prompt vault for execution.",
            "raw_prompt_available": true,
            "content_redacted": true,
            "ciphertext_bytes": ciphertext_bytes,
            "plaintext_bytes": plaintext_bytes
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: vec![
            "conversation:raw-prompt-required".to_string(),
            "conversation:raw-prompt-vault-decrypt".to_string(),
        ],
    }
}

fn raw_prompt_unavailable_after_redaction_agent_event(
    request: &opensks_adapter::AgentRunRequest,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("prompt-vault-policy".to_string()),
        node_id: None,
        sequence: 0,
        occurred_at_ms: request.now_ms,
        kind: opensks_contracts::AgentEventKind::Error,
        payload: serde_json::json!({
            "code": "raw_prompt_unavailable_after_redaction",
            "message": "Raw prompt content is required because the durable searchable prompt is redacted.",
            "raw_prompt_available": false,
            "content_redacted": true
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: vec![
            "conversation:raw-prompt-required".to_string(),
            "conversation:redacted-prompt-fail-closed".to_string(),
        ],
    }
}

fn read_only_execution_mode_agent_event(
    request: &opensks_adapter::AgentRunRequest,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("execution-mode-policy".to_string()),
        node_id: None,
        sequence: 0,
        occurred_at_ms: request.now_ms,
        kind: opensks_contracts::AgentEventKind::Error,
        payload: serde_json::json!({
            "code": "read_only_execution_mode",
            "message": "Read-only execution mode blocked workspace writes for this turn."
        }),
        sensitivity: opensks_contracts::Sensitivity::Public,
        evidence_refs: vec!["settings:execution_mode:read_only".to_string()],
    }
}

fn provider_dispatch_unavailable_agent_event(
    request: &opensks_adapter::AgentRunRequest,
    message: &str,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("provider-dispatch".to_string()),
        node_id: None,
        sequence: 0,
        occurred_at_ms: request.now_ms,
        kind: opensks_contracts::AgentEventKind::Error,
        payload: serde_json::json!({
            "code": "provider_dispatch_unavailable",
            "message": message
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: vec!["provider:dispatch-preflight".to_string()],
    }
}

fn execution_workspace_prepared_agent_event(
    request: &opensks_adapter::AgentRunRequest,
    isolation: &opensks_contracts::GitIsolationReport,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("worktree-isolation".to_string()),
        node_id: None,
        sequence: 0,
        occurred_at_ms: request.now_ms,
        kind: opensks_contracts::AgentEventKind::WorkerSpawned,
        payload: serde_json::json!({
            "code": "execution_workspace_prepared",
            "isolation_id": isolation.id,
            "isolation_mode": isolation_mode_label(&isolation.mode),
            "reason_code": isolation.reason_code,
            "git_available": isolation.git_available,
            "base_commit": isolation.base_commit,
            "submodule_detected": isolation.submodule_detected,
            "lfs_detected": isolation.lfs_detected,
            "path_redacted": true
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: vec!["git:isolation-prepared".to_string()],
    }
}

fn integration_candidate_agent_event(
    request: &opensks_adapter::AgentRunRequest,
    candidate: &IntegrationCandidateReceipt,
) -> opensks_contracts::AgentEventEnvelope {
    opensks_contracts::AgentEventEnvelope {
        schema: opensks_contracts::AGENT_EVENT_ENVELOPE_SCHEMA.to_string(),
        stream_id: request.stream_id.clone(),
        project_id: request.project_id.clone(),
        conversation_id: request.conversation_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: request.run_id.clone(),
        worker_id: Some("integration-coordinator".to_string()),
        node_id: None,
        sequence: 0,
        occurred_at_ms: request.now_ms,
        kind: opensks_contracts::AgentEventKind::WorkerProgress,
        payload: serde_json::json!({
            "code": "integration_candidate_ready",
            "candidate_id": candidate.id,
            "state": candidate.state,
            "reason_code": candidate.reason_code,
            "target_count": candidate.target_paths.len(),
            "aggregate_candidate_count": candidate.aggregate_candidate_count,
            "aggregate_target_count": candidate.aggregate_target_count,
            "source_candidate_count": candidate.source_candidates.len(),
            "planned_verifier_count": candidate.planned_verifier_count,
            "receipt_ref": candidate.receipt_ref,
            "patch_ref": candidate.patch_ref,
            "selection_ref": candidate.selection_ref.as_deref(),
            "main_workspace_modified": false,
            "approval_required": true,
            "path_redacted": true,
            "content_redacted": true
        }),
        sensitivity: opensks_contracts::Sensitivity::Internal,
        evidence_refs: vec![
            "integration:candidate-ready".to_string(),
            "integration:candidate-selection-receipt".to_string(),
        ],
    }
}

fn isolation_mode_label(mode: &opensks_contracts::IsolationMode) -> String {
    serde_json::to_value(mode)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
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
    } else if pipeline_id == "objective-planner" {
        let mut plan_request = opensks_contracts::ObjectivePlanRequest::new(objective);
        plan_request.evidence_refs = vec!["daemon:objective-planner-run-start".to_string()];
        let planned = opensks_graph::plan_graph_from_objective(&plan_request);
        opensks_engine::run_graph_with_event_stream(
            &options.workspace,
            &run_id,
            &planned.graph,
            objective,
            "daemon:objective-planner-run-start",
            "objective_planner",
        )
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
            let projected =
                project_run_control_event_to_timeline(&options.workspace, run_id, &result.event)?;
            let mut accepted = EngineEvent::new(
                format!("engine-run-control-{}-{}", run_id, result.event.sequence),
                Some(request.id.clone()),
                EngineEventType::ExecutionEvent,
                format!("run control accepted: {}", result.event.kind.as_str()),
                timestamp_ms(),
            );
            accepted.evidence_refs = vec!["daemon:run-control".to_string()];
            if projected {
                accepted
                    .evidence_refs
                    .push("conversation:run-control-projected".to_string());
            }
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
        EngineRequestParams, ExecutionMode, HealthState, MODEL_CATALOG_ENTRY_SCHEMA, MessageRole,
        MessageState, ModelCapabilities, ModelCatalogEntry, ModelLimits, ModelRole, ModelSelection,
        ModelSelectionMode, PROVIDER_CONNECTION_SCHEMA, ProviderConcurrencyPolicy,
        ProviderConnection, ProviderEndpoint, ProviderHealthSnapshot, ProviderKind,
        ReasoningEffort, RoleScore, RunProjectionState, SECRET_REF_SCHEMA, SecretRef,
        SecretStoreKind, TurnContextSelection, UserMessageInput,
    };
    use opensks_conversation::ConversationRepository;
    use std::collections::BTreeMap;
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn temp_workspace(name: &str) -> PathBuf {
        let workspace =
            std::env::temp_dir().join(format!("opensks-daemon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        workspace
    }

    fn run_git(workspace: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(workspace)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn test_openai_key_assignment(value: &str) -> String {
        format!("{}={value}", "OPENAI_API_KEY")
    }

    fn local_test_secret_write_prompt(path: &str, value: &str) -> String {
        serde_json::json!({
            "local_test": {
                "op": "create_file",
                "path": path,
                "value": test_openai_key_assignment(value)
            }
        })
        .to_string()
    }

    fn fnv1a64(bytes: &[u8]) -> String {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("fnv1a64:{hash:016x}")
    }

    fn git_workspace(name: &str) -> PathBuf {
        let workspace = temp_workspace(name);
        run_git(&workspace, &["init"]);
        run_git(
            &workspace,
            &["config", "user.email", "opensks@example.test"],
        );
        run_git(&workspace, &["config", "user.name", "OpenSKS Test"]);
        std::fs::write(workspace.join("NOTE.md"), "before\n").expect("seed note");
        run_git(&workspace, &["add", "NOTE.md"]);
        run_git(&workspace, &["commit", "-m", "initial"]);
        workspace
    }

    fn seed_objective_runtime_targets(workspace: &Path) {
        std::fs::write(workspace.join("ROUTED_NOTE.md"), "before routed\n")
            .expect("seed routed note");
        std::fs::write(workspace.join("ROLE_CODE_NOTE.md"), "before role\n")
            .expect("seed role note");
        run_git(workspace, &["add", "ROUTED_NOTE.md", "ROLE_CODE_NOTE.md"]);
        run_git(workspace, &["commit", "-m", "seed objective targets"]);
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

    fn seed_healthy_provider_model_with_endpoint(
        workspace: &Path,
        endpoint: &str,
        allow_insecure_http: bool,
    ) {
        let repo =
            opensks_provider::ProviderRepository::open_workspace(workspace).expect("provider repo");
        let connection = ProviderConnection {
            schema: PROVIDER_CONNECTION_SCHEMA.to_string(),
            id: "provider-1".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            display_name: "Provider One".to_string(),
            enabled: true,
            endpoint: ProviderEndpoint {
                base_url: endpoint.to_string(),
                allow_insecure_http,
            },
            auth: SecretRef {
                schema: SECRET_REF_SCHEMA.to_string(),
                store: SecretStoreKind::TestMemory,
                service: "ai.opensks.provider.test".to_string(),
                account: "provider-1".to_string(),
                version: 1,
            },
            organization_ref: None,
            project_ref: None,
            health: ProviderHealthSnapshot {
                state: HealthState::Healthy,
                circuit_open: false,
                checked_at_ms: Some(1_150),
                reason_code: "probe_ok".to_string(),
                diagnostic_ref: None,
            },
            concurrency: ProviderConcurrencyPolicy {
                max_concurrent_requests: 2,
                requests_per_minute: Some(60),
                tokens_per_minute: None,
            },
            created_at_ms: 1_100,
            updated_at_ms: 1_150,
            revision: 1,
        };
        repo.upsert_connection(&connection, None, 1_150)
            .expect("provider connection");
        let mut role_scores = BTreeMap::new();
        role_scores.insert(
            ModelRole::Code,
            RoleScore {
                score: 0.9,
                evidence_refs: vec!["daemon-test-registry".to_string()],
            },
        );
        let model = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/code-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "code-model".to_string(),
            display_name: "Code Model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities::text_code(),
            limits: ModelLimits {
                max_input_tokens: Some(64_000),
                max_output_tokens: Some(8_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(2),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores,
            catalog_revision: "daemon-test-catalog".to_string(),
        };
        repo.sync_models("provider-1", &[model], 1_160)
            .expect("model catalog");
    }

    fn seed_healthy_image_provider_model_with_endpoint(
        workspace: &Path,
        endpoint: &str,
        allow_insecure_http: bool,
    ) {
        let repo =
            opensks_provider::ProviderRepository::open_workspace(workspace).expect("provider repo");
        let connection = ProviderConnection {
            schema: PROVIDER_CONNECTION_SCHEMA.to_string(),
            id: "provider-1".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            display_name: "Provider One".to_string(),
            enabled: true,
            endpoint: ProviderEndpoint {
                base_url: endpoint.to_string(),
                allow_insecure_http,
            },
            auth: SecretRef {
                schema: SECRET_REF_SCHEMA.to_string(),
                store: SecretStoreKind::TestMemory,
                service: "ai.opensks.provider.test".to_string(),
                account: "provider-1".to_string(),
                version: 1,
            },
            organization_ref: None,
            project_ref: None,
            health: ProviderHealthSnapshot {
                state: HealthState::Healthy,
                circuit_open: false,
                checked_at_ms: Some(1_150),
                reason_code: "probe_ok".to_string(),
                diagnostic_ref: None,
            },
            concurrency: ProviderConcurrencyPolicy {
                max_concurrent_requests: 2,
                requests_per_minute: Some(60),
                tokens_per_minute: None,
            },
            created_at_ms: 1_100,
            updated_at_ms: 1_150,
            revision: 1,
        };
        repo.upsert_connection(&connection, None, 1_150)
            .expect("provider connection");
        let mut role_scores = BTreeMap::new();
        role_scores.insert(
            ModelRole::Image,
            RoleScore {
                score: 0.9,
                evidence_refs: vec!["daemon-test-image-registry".to_string()],
            },
        );
        let model = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/image-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "gpt-image-1.5".to_string(),
            display_name: "Image Model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities::image(),
            limits: ModelLimits {
                max_input_tokens: Some(64_000),
                max_output_tokens: Some(8_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(2),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores,
            catalog_revision: "daemon-test-image-catalog".to_string(),
        };
        repo.sync_models("provider-1", &[model], 1_160)
            .expect("image model catalog");
    }

    fn seed_healthy_image_and_vision_provider_models_with_endpoint(
        workspace: &Path,
        endpoint: &str,
        allow_insecure_http: bool,
    ) {
        seed_healthy_image_provider_model_with_endpoint(workspace, endpoint, allow_insecure_http);
        let repo =
            opensks_provider::ProviderRepository::open_workspace(workspace).expect("provider repo");
        let mut image_scores = BTreeMap::new();
        image_scores.insert(
            ModelRole::Image,
            RoleScore {
                score: 0.9,
                evidence_refs: vec!["daemon-test-image-registry".to_string()],
            },
        );
        let image_model = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/image-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "gpt-image-1.5".to_string(),
            display_name: "Image Model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities::image(),
            limits: ModelLimits {
                max_input_tokens: Some(64_000),
                max_output_tokens: Some(8_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(2),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores: image_scores,
            catalog_revision: "daemon-test-image-catalog".to_string(),
        };
        let mut vision_scores = BTreeMap::new();
        vision_scores.insert(
            ModelRole::Vision,
            RoleScore {
                score: 0.9,
                evidence_refs: vec!["daemon-test-vision-registry".to_string()],
            },
        );
        let vision_model = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/vision-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "gpt-vision-1.5".to_string(),
            display_name: "Vision Model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities {
                text: true,
                vision_input: true,
                structured_output: true,
                ..ModelCapabilities::default()
            },
            limits: ModelLimits {
                max_input_tokens: Some(64_000),
                max_output_tokens: Some(8_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(2),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores: vision_scores,
            catalog_revision: "daemon-test-vision-catalog".to_string(),
        };
        repo.sync_models("provider-1", &[image_model, vision_model], 1_170)
            .expect("image and vision model catalog");
    }

    fn seed_healthy_code_image_vision_provider_models_with_endpoint(
        workspace: &Path,
        endpoint: &str,
        allow_insecure_http: bool,
    ) {
        seed_healthy_provider_model_with_endpoint(workspace, endpoint, allow_insecure_http);
        let repo =
            opensks_provider::ProviderRepository::open_workspace(workspace).expect("provider repo");
        let mut code_scores = BTreeMap::new();
        code_scores.insert(
            ModelRole::Code,
            RoleScore {
                score: 0.9,
                evidence_refs: vec!["daemon-test-registry".to_string()],
            },
        );
        let code_model = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/code-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "code-model".to_string(),
            display_name: "Code Model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities::text_code(),
            limits: ModelLimits {
                max_input_tokens: Some(64_000),
                max_output_tokens: Some(8_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(2),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores: code_scores,
            catalog_revision: "daemon-test-catalog".to_string(),
        };
        let mut auto_image_scores = BTreeMap::new();
        auto_image_scores.insert(
            ModelRole::Image,
            RoleScore {
                score: 0.9,
                evidence_refs: vec!["daemon-test-image-registry".to_string()],
            },
        );
        let auto_image_model = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/image-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "gpt-image-auto".to_string(),
            display_name: "Auto Image Model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities::image(),
            limits: ModelLimits {
                max_input_tokens: Some(64_000),
                max_output_tokens: Some(8_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(2),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores: auto_image_scores,
            catalog_revision: "daemon-test-image-catalog".to_string(),
        };
        let mut selected_image_scores = BTreeMap::new();
        selected_image_scores.insert(
            ModelRole::Image,
            RoleScore {
                score: 0.1,
                evidence_refs: vec!["daemon-test-selected-image-registry".to_string()],
            },
        );
        let selected_image_model = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/selected-image-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "gpt-image-selected".to_string(),
            display_name: "Selected Image Model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities::image(),
            limits: ModelLimits {
                max_input_tokens: Some(64_000),
                max_output_tokens: Some(8_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(2),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores: selected_image_scores,
            catalog_revision: "daemon-test-selected-image-catalog".to_string(),
        };
        let mut vision_scores = BTreeMap::new();
        vision_scores.insert(
            ModelRole::Vision,
            RoleScore {
                score: 0.9,
                evidence_refs: vec!["daemon-test-vision-registry".to_string()],
            },
        );
        let vision_model = ModelCatalogEntry {
            schema: MODEL_CATALOG_ENTRY_SCHEMA.to_string(),
            id: "provider-1/vision-model".to_string(),
            provider_id: "provider-1".to_string(),
            remote_model_id: "gpt-vision-1.5".to_string(),
            display_name: "Vision Model".to_string(),
            enabled: true,
            capabilities: ModelCapabilities {
                text: true,
                vision_input: true,
                structured_output: true,
                ..ModelCapabilities::default()
            },
            limits: ModelLimits {
                max_input_tokens: Some(64_000),
                max_output_tokens: Some(8_000),
                requests_per_minute: Some(60),
                tokens_per_minute: None,
                max_concurrency: Some(2),
            },
            pricing: None,
            health: HealthState::Healthy,
            role_scores: vision_scores,
            catalog_revision: "daemon-test-vision-catalog".to_string(),
        };
        repo.sync_models(
            "provider-1",
            &[
                code_model,
                auto_image_model,
                selected_image_model,
                vision_model,
            ],
            1_180,
        )
        .expect("code image vision model catalog");
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
            thread_settings_updated_at_ms: None,
            settings: Some(ConversationTurnSettings {
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
            }),
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

    #[test]
    fn live_objective_planner_directive_clamps_to_dispatchable_runtime_lanes() {
        let settings = ConversationTurnSettings {
            model: ModelSelection {
                mode: ModelSelectionMode::Auto,
                model_id: None,
                fallback_model_ids: Vec::new(),
            },
            reasoning_effort: ReasoningEffort::Standard,
            execution_mode: ExecutionMode::Worktree,
            pipeline_id: "objective-planner".to_string(),
            graph_revision: None,
            max_parallelism: 4,
            verifier_count: 1,
            tool_policy_id: "project-default".to_string(),
            approval_policy_id: "safe-interactive".to_string(),
            token_budget: None,
            cost_budget_usd: None,
            timeout_ms: None,
            image_model_id: None,
        };
        let mut request =
            opensks_contracts::ObjectivePlanRequest::new("Implement provider routing".to_string());
        apply_objective_planner_directive(
            &settings,
            &mut request,
            ObjectivePlannerDirective {
                max_parallelism: Some(99),
                role_count: Some(8),
                include_image_lane: Some(true),
                include_research_lane: Some(true),
            },
        );

        assert_eq!(request.max_parallelism, 4);
        assert_eq!(request.role_count, 4);
        assert!(!request.include_image_lane);
        assert!(!request.include_research_lane);
        assert!(request.require_git_worktree);
        assert!(request.require_integration_approval);
        assert!(
            request
                .evidence_refs
                .contains(&"daemon:objective-plan-optional-lanes-clamped".to_string())
        );
    }

    #[test]
    fn objective_planner_model_call_body_includes_provider_reasoning_effort() {
        let request =
            opensks_contracts::ObjectivePlanRequest::new("Implement provider routing".to_string());
        assert_eq!(
            chat_reasoning_effort_wire_for_provider(ProviderKind::OpenRouter),
            Some(opensks_adapter::ChatReasoningEffortWire::OpenRouterReasoningObject)
        );
        assert_eq!(
            chat_reasoning_effort_wire_for_provider(ProviderKind::OpenAi),
            Some(opensks_adapter::ChatReasoningEffortWire::OpenAiReasoningEffort)
        );
        assert_eq!(
            chat_reasoning_effort_wire_for_provider(ProviderKind::OpenAiCompatible),
            None
        );
        let body = objective_planner_model_call_body(
            "openrouter/test-model",
            512,
            &request,
            provider_reasoning_effort_for_provider(
                ProviderKind::OpenRouter,
                ReasoningEffort::Maximum,
            ),
        );

        assert_eq!(body["model"], "openrouter/test-model");
        assert_eq!(body["reasoning"]["effort"], "xhigh");
        assert_eq!(body["max_tokens"], 256);

        let openai_body = objective_planner_model_call_body(
            "gpt-test",
            512,
            &request,
            provider_reasoning_effort_for_provider(ProviderKind::OpenAi, ReasoningEffort::Deep),
        );
        assert_eq!(openai_body["reasoning_effort"], "high");
        assert!(openai_body.get("reasoning").is_none());
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

    fn integration_apply_receipt_lines(
        output: &str,
    ) -> Vec<opensks_contracts::IntegrationApplyReceipt> {
        output
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                value.get("schema").and_then(serde_json::Value::as_str)
                    == Some(opensks_contracts::INTEGRATION_APPLY_RECEIPT_SCHEMA)
            })
            .map(|value| serde_json::from_value(value).expect("integration apply receipt"))
            .collect()
    }

    fn integration_turn_settings_fixture() -> opensks_contracts::IntegrationTurnSettingsSnapshot {
        let settings = opensks_contracts::ConversationTurnSettings {
            model: opensks_contracts::ModelSelection {
                mode: opensks_contracts::turn::ModelSelectionMode::Pinned,
                model_id: Some("provider-1/code-model".to_string()),
                fallback_model_ids: vec!["provider-1/fallback-code".to_string()],
            },
            reasoning_effort: opensks_contracts::ReasoningEffort::Deep,
            execution_mode: opensks_contracts::ExecutionMode::Worktree,
            pipeline_id: "integration-test-pipeline".to_string(),
            graph_revision: Some("graph-rev-1".to_string()),
            max_parallelism: 6,
            verifier_count: 3,
            tool_policy_id: "integration-tools".to_string(),
            approval_policy_id: "safe-interactive".to_string(),
            token_budget: Some(12_000),
            cost_budget_usd: Some(2.5),
            timeout_ms: Some(45_000),
            image_model_id: Some("provider-1/image-model".to_string()),
        };
        opensks_contracts::IntegrationTurnSettingsSnapshot::from(&settings)
    }

    fn write_candidate_fixture(workspace: &Path, run_id: &str, target: &str, patch: &str) {
        let candidate_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(run_id);
        std::fs::create_dir_all(&candidate_dir).expect("candidate dir");
        let head = run_git(workspace, &["rev-parse", "HEAD"]);
        let turn_settings = serde_json::to_value(integration_turn_settings_fixture())
            .expect("turn settings fixture");
        let candidate = serde_json::json!({
            "schema": "opensks.integration-candidate.v1",
            "id": format!("integration-candidate-{run_id}"),
            "run_id": run_id,
            "turn_id": "turn-fixture",
            "conversation_id": "conversation-fixture",
            "project_id": "project-fixture",
            "worker_id": "turn-supervisor",
            "state": "candidate_ready",
            "reason_code": "isolated_patch_candidate_ready",
            "source_isolation_id": format!("isolation-{run_id}-turn-supervisor"),
            "source_isolation_mode": "git_worktree",
            "source_base_commit": head,
            "source_git_available": true,
            "source_candidates": [{
                "source": "turn_supervisor",
                "id": format!("turn-supervisor-candidate-{run_id}"),
                "receipt_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.json"),
                "patch_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.patch"),
                "worker_id": "turn-supervisor",
                "work_item_id": null,
                "role": null,
                "source_isolation_id": format!("isolation-{run_id}-turn-supervisor"),
                "source_isolation_mode": "git_worktree",
                "target_paths": [target]
            }],
            "aggregate_candidate_count": 1,
            "aggregate_target_count": 1,
            "planned_verifier_count": 1,
            "target_paths": [target],
            "patch_count": 1,
            "apply_result_count": 1,
            "applied_files": [target],
            "receipt_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.json"),
            "patch_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.patch"),
            "main_workspace_modified": false,
            "integration_required": true,
            "approval_required": true,
            "approval_policy_id": "safe-interactive",
            "turn_settings": turn_settings,
            "path_redacted": true,
            "content_redacted": true,
            "generated_at_ms": 1_000,
            "evidence_refs": [
                "git:isolation-prepared",
                "patch-engine:atomic-apply",
                "integration:candidate-ready",
                "settings:approval-policy",
                "settings:turn-settings-snapshot"
            ]
        });
        std::fs::write(
            candidate_dir.join("candidate.json"),
            serde_json::to_string_pretty(&candidate).expect("candidate json"),
        )
        .expect("write candidate");
        let selection = serde_json::json!({
            "schema": "opensks.integration-candidate-selection-receipt.v1",
            "id": format!("integration-candidate-selection-{run_id}"),
            "run_id": run_id,
            "selected_candidate_id": format!("integration-candidate-{run_id}"),
            "selection_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/selection.json"),
            "selected_candidate_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.json"),
            "selected_patch_ref": format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.patch"),
            "candidate_pool": candidate["source_candidates"].clone(),
            "selected_source_candidate_ids": [format!("turn-supervisor-candidate-{run_id}")],
            "selection_policy": "deterministic_single_ready_candidate",
            "reason_code": "single_ready_source_selected",
            "required_verification_gates": [
                "candidate_receipt_valid",
                "target_policy_check",
                "patch_apply_check",
                "read_only_verifier_lanes",
                "approval_event"
            ],
            "aggregate_candidate_count": 1,
            "aggregate_target_count": 1,
            "planned_verifier_count": 1,
            "approval_policy_id": "safe-interactive",
            "turn_settings": candidate["turn_settings"].clone(),
            "target_paths": [target],
            "path_redacted": true,
            "content_redacted": true,
            "evidence_refs": [
                "integration:candidate-selection-receipt",
                "schema:integration-candidate-selection-receipt",
                "settings:approval-policy"
            ],
            "generated_at_ms": 1_000
        });
        std::fs::write(
            candidate_dir.join("selection.json"),
            serde_json::to_string_pretty(&selection).expect("selection json"),
        )
        .expect("write selection");
        std::fs::write(candidate_dir.join("candidate.patch"), patch).expect("write patch");
    }

    fn write_aggregate_candidate_fixture(
        workspace: &Path,
        run_id: &str,
        targets: &[&str],
        patch: &str,
    ) {
        let candidate_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(run_id);
        std::fs::create_dir_all(&candidate_dir).expect("candidate dir");
        let head = run_git(workspace, &["rev-parse", "HEAD"]);
        let receipt_ref =
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.json");
        let patch_ref =
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/candidate.patch");
        let target_paths = targets.iter().map(|target| target.to_string()).collect();
        let candidate = opensks_contracts::IntegrationCandidateReceipt {
            schema: opensks_contracts::INTEGRATION_CANDIDATE_RECEIPT_SCHEMA.to_string(),
            id: format!("integration-candidate-{run_id}"),
            run_id: run_id.to_string(),
            turn_id: "turn-fixture".to_string(),
            conversation_id: "conversation-fixture".to_string(),
            project_id: "project-fixture".to_string(),
            worker_id: "integration-coordinator".to_string(),
            state: "candidate_ready".to_string(),
            reason_code: "aggregate_isolated_patch_candidate_ready".to_string(),
            source_isolation_id: Some(format!("isolation-{run_id}-turn-supervisor")),
            source_isolation_mode: Some("git_worktree".to_string()),
            source_base_commit: Some(head),
            source_git_available: true,
            source_candidates: vec![
                opensks_contracts::IntegrationSourceCandidateRef {
                    source: "turn_supervisor".to_string(),
                    id: format!("turn-supervisor-candidate-{run_id}"),
                    receipt_ref: receipt_ref.clone(),
                    patch_ref: patch_ref.clone(),
                    worker_id: "turn-supervisor".to_string(),
                    work_item_id: None,
                    role: None,
                    source_isolation_id: Some(format!("isolation-{run_id}-turn-supervisor")),
                    source_isolation_mode: Some("git_worktree".to_string()),
                    target_paths: vec![targets[0].to_string()],
                    shard_policy_id: None,
                    shard_policy_selection_policy: None,
                    planner_required_source_candidate_count: 0,
                    planner_required_verifier_count: 0,
                },
                opensks_contracts::IntegrationSourceCandidateRef {
                    source: "role_subcontract".to_string(),
                    id: format!("role-candidate-{run_id}-code"),
                    receipt_ref: format!(
                        "artifact://.opensks/runtime/role-candidates/{run_id}/turn-role-code/candidate.json"
                    ),
                    patch_ref: format!(
                        "artifact://.opensks/runtime/role-candidates/{run_id}/turn-role-code/candidate.patch"
                    ),
                    worker_id: "role-subcontract-turn-role-code".to_string(),
                    work_item_id: Some("turn-role-code".to_string()),
                    role: Some("code".to_string()),
                    source_isolation_id: Some(format!(
                        "isolation-{run_id}-role-subcontract-turn-role-code"
                    )),
                    source_isolation_mode: Some("git_worktree".to_string()),
                    target_paths: vec![targets[1].to_string()],
                    shard_policy_id: None,
                    shard_policy_selection_policy: None,
                    planner_required_source_candidate_count: 0,
                    planner_required_verifier_count: 0,
                },
            ],
            aggregate_candidate_count: 2,
            aggregate_target_count: targets.len(),
            planned_verifier_count: 3,
            shard_policy_id: None,
            shard_policy_selection_policy: None,
            planner_required_source_candidate_count: 0,
            planner_selected_source_candidate_count: 0,
            planner_required_verifier_count: 0,
            target_paths,
            patch_count: 2,
            apply_result_count: 2,
            applied_files: targets.iter().map(|target| target.to_string()).collect(),
            receipt_ref,
            patch_ref,
            selection_ref: None,
            main_workspace_modified: false,
            integration_required: true,
            approval_required: true,
            approval_policy_id: Some("safe-interactive".to_string()),
            turn_settings: Some(integration_turn_settings_fixture()),
            path_redacted: true,
            content_redacted: true,
            generated_at_ms: 1_000,
            evidence_refs: vec![
                "integration:candidate-ready".to_string(),
                "integration:role-candidate-aggregate".to_string(),
                "integration:aggregate-candidate-ready".to_string(),
            ],
        };
        std::fs::write(
            candidate_dir.join("candidate.json"),
            serde_json::to_string_pretty(&candidate).expect("candidate json"),
        )
        .expect("write candidate");
        std::fs::write(candidate_dir.join("candidate.patch"), patch).expect("write patch");
    }

    fn add_planner_policy_to_candidate_fixture(
        workspace: &Path,
        run_id: &str,
        required_source_candidate_count: usize,
        selected_source_candidate_count: usize,
        required_verifier_count: usize,
    ) {
        let candidate_path = workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(run_id)
            .join("candidate.json");
        let mut candidate: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&candidate_path).expect("candidate fixture"),
        )
        .expect("candidate json");
        let policy_id = format!("planner-shard-policy-{run_id}");
        let policy = "planner_required_shards_before_approval_apply";
        candidate["shard_policy_id"] = serde_json::json!(policy_id);
        candidate["shard_policy_selection_policy"] = serde_json::json!(policy);
        candidate["planner_required_source_candidate_count"] =
            serde_json::json!(required_source_candidate_count);
        candidate["planner_selected_source_candidate_count"] =
            serde_json::json!(selected_source_candidate_count);
        candidate["planner_required_verifier_count"] = serde_json::json!(required_verifier_count);
        let shard_policy_id = candidate["shard_policy_id"].clone();
        let shard_policy_selection_policy = candidate["shard_policy_selection_policy"].clone();
        if let Some(source) = candidate["source_candidates"]
            .as_array_mut()
            .and_then(|sources| sources.first_mut())
        {
            source["shard_policy_id"] = shard_policy_id;
            source["shard_policy_selection_policy"] = shard_policy_selection_policy;
            source["planner_required_source_candidate_count"] =
                serde_json::json!(required_source_candidate_count);
            source["planner_required_verifier_count"] = serde_json::json!(required_verifier_count);
        }
        std::fs::write(
            candidate_path,
            serde_json::to_string_pretty(&candidate).expect("candidate json"),
        )
        .expect("write candidate");
    }

    fn write_semantic_verifier_judgment_fixture(workspace: &Path, run_id: &str, verdict: &str) {
        let work_item_id = "turn-role-fixture-verification";
        let judgment_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("semantic-verifiers")
            .join(run_id)
            .join(work_item_id);
        std::fs::create_dir_all(&judgment_dir).expect("semantic verifier dir");
        let (passed_gates, failed_gates) = if verdict == "pass" {
            (
                vec!["semantic_verifier_verdict_passed".to_string()],
                Vec::new(),
            )
        } else {
            (
                Vec::new(),
                vec!["semantic_verifier_verdict_passed".to_string()],
            )
        };
        let receipt = opensks_contracts::SemanticVerifierJudgmentReceipt {
            schema: opensks_contracts::SEMANTIC_VERIFIER_JUDGMENT_SCHEMA.to_string(),
            id: format!("semantic-verifier-{run_id}-{work_item_id}"),
            run_id: run_id.to_string(),
            work_item_id: work_item_id.to_string(),
            role: "verification".to_string(),
            worker_id: "semantic-verifier-fixture".to_string(),
            state: "judgment_ready".to_string(),
            reason_code: "model_semantic_verifier_judgment_recorded".to_string(),
            verifier_kind: "model_semantic_judgment".to_string(),
            verdict: verdict.to_string(),
            passed_gates,
            failed_gates,
            provider_id: Some("provider-1".to_string()),
            model_id: Some("provider-1/code-model".to_string()),
            response_hash: "fnv64:semanticfixture0001".to_string(),
            response_bytes: 24,
            judgment_ref: format!(
                "artifact://.opensks/runtime/semantic-verifiers/{run_id}/{work_item_id}/judgment.json"
            ),
            context_pack_ref: None,
            worker_context_pack_ref: None,
            path_redacted: true,
            content_redacted: true,
            generated_at_ms: 1_000,
            evidence_refs: vec!["daemon:semantic-verifier-judgment".to_string()],
        };
        std::fs::write(
            judgment_dir.join("judgment.json"),
            serde_json::to_string_pretty(&receipt).expect("semantic verifier fixture"),
        )
        .expect("write semantic verifier fixture");
    }

    fn integration_artifact_path(workspace: &Path, run_id: &str, name: &str) -> PathBuf {
        workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(run_id)
            .join(name)
    }

    fn source_isolation_path(workspace: &Path, run_id: &str, worker_id: &str) -> PathBuf {
        workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(run_id)
            .join(worker_id)
    }

    fn worktree_isolation_request(
        kind: &str,
        request_id: &str,
        workspace: &Path,
        run_id: &str,
    ) -> String {
        serde_json::json!({
            "schema": opensks_contracts::ENGINE_REQUEST_SCHEMA,
            "id": request_id,
            "kind": kind,
            "params": {
                "workspace": workspace.to_string_lossy(),
                "run_id": run_id
            }
        })
        .to_string()
    }

    #[test]
    fn worktree_isolation_inventory_request_writes_redacted_receipt_and_event() {
        let workspace = temp_workspace("worktree-isolation-inventory");
        let run_id = "run-daemon-worktree-inventory";
        let secret = format!("{}=sk-daemon-inventory-secret", "OPENAI_API_KEY");
        let isolation = source_isolation_path(&workspace, run_id, "turn-supervisor");
        std::fs::create_dir_all(&isolation).expect("source isolation");
        std::fs::write(isolation.join("SECRET.txt"), secret.as_bytes()).expect("secret fixture");

        let request = worktree_isolation_request(
            WORKTREE_ISOLATION_INVENTORY_KIND,
            "req-worktree-inventory",
            &workspace,
            run_id,
        );
        let output = run_stdio(
            &(request + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("inventory stdio");
        assert!(output.contains("opensks.worktree-isolation-inventory-receipt.v1"));
        assert!(output.contains("\"daemon:worktree-isolation-inventory\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        assert!(!output.contains(secret.as_str()));

        let receipt_path = worktree_isolation_artifact_path(&workspace, run_id, "inventory.json");
        let receipt_raw = std::fs::read_to_string(receipt_path).expect("inventory receipt");
        assert!(!receipt_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!receipt_raw.contains(secret.as_str()));
        let receipt: opensks_contracts::WorktreeIsolationInventoryReceipt =
            serde_json::from_str(&receipt_raw).expect("inventory json");
        assert_eq!(receipt.run_id, run_id);
        assert_eq!(receipt.state, "present");
        assert_eq!(receipt.isolation_count, 1);
        assert!(receipt.path_redacted);
        assert!(receipt.content_redacted);
        assert_eq!(receipt.isolations[0].worker_id, "turn-supervisor");
        assert_eq!(
            receipt.isolations[0].artifact_ref,
            format!("artifact://.opensks/runtime/worktrees/{run_id}/turn-supervisor")
        );

        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(run_id)
            .expect("events");
        let events_raw = serde_json::to_string(&events).expect("events json");
        assert!(events_raw.contains("daemon.worktree_isolation_inventory"));
        assert!(!events_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!events_raw.contains(secret.as_str()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn worktree_isolation_recovery_request_writes_receipts_and_removes_isolations() {
        let workspace = temp_workspace("worktree-isolation-recovery");
        let run_id = "run-daemon-worktree-recovery";
        let secret = "SERVICE_TOKEN=sk-daemon-recovery-secret";
        let turn_isolation = source_isolation_path(&workspace, run_id, "turn-supervisor");
        let role_isolation = source_isolation_path(&workspace, run_id, "role-subcontract-code");
        std::fs::create_dir_all(&turn_isolation).expect("turn isolation");
        std::fs::create_dir_all(&role_isolation).expect("role isolation");
        std::fs::write(turn_isolation.join(".env"), secret).expect("turn secret fixture");
        std::fs::write(role_isolation.join("NOTE.md"), "candidate\n").expect("role fixture");

        let request = worktree_isolation_request(
            WORKTREE_ISOLATION_RECOVERY_KIND,
            "req-worktree-recovery",
            &workspace,
            run_id,
        );
        let output = run_stdio(
            &(request + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("recovery stdio");
        assert!(output.contains("opensks.worktree-isolation-inventory-receipt.v1"));
        assert!(output.contains("opensks.worktree-isolation-recovery-receipt.v1"));
        assert!(output.contains("\"daemon:worktree-isolation-recovery\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        assert!(!output.contains(secret));
        assert!(!turn_isolation.exists());
        assert!(!role_isolation.exists());

        let inventory_raw = std::fs::read_to_string(worktree_isolation_artifact_path(
            &workspace,
            run_id,
            "inventory.json",
        ))
        .expect("inventory receipt");
        let recovery_raw = std::fs::read_to_string(worktree_isolation_artifact_path(
            &workspace,
            run_id,
            "recovery.json",
        ))
        .expect("recovery receipt");
        assert!(!inventory_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!inventory_raw.contains(secret));
        assert!(!recovery_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!recovery_raw.contains(secret));
        let recovery: opensks_contracts::WorktreeIsolationRecoveryReceipt =
            serde_json::from_str(&recovery_raw).expect("recovery json");
        assert_eq!(recovery.run_id, run_id);
        assert_eq!(recovery.state, "recovered");
        assert_eq!(recovery.target_count, 2);
        assert_eq!(recovery.recovered_count, 2);
        assert!(recovery.path_redacted);
        assert!(recovery.content_redacted);

        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(run_id)
            .expect("events");
        let events_raw = serde_json::to_string(&events).expect("events json");
        assert!(events_raw.contains("daemon.worktree_isolation_recovery"));
        assert!(!events_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!events_raw.contains(secret));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn worktree_isolation_request_rejects_path_traversal_without_leaking_inputs() {
        let workspace = temp_workspace("worktree-isolation-rejects-traversal");
        let bad_run_id = "../run-secret-token";
        let bad_workspace = workspace.join("..");
        let bad_run_request = worktree_isolation_request(
            WORKTREE_ISOLATION_INVENTORY_KIND,
            "req-worktree-bad-run",
            &workspace,
            bad_run_id,
        );
        let bad_workspace_request = worktree_isolation_request(
            WORKTREE_ISOLATION_INVENTORY_KIND,
            "req-worktree-bad-workspace",
            &bad_workspace,
            "run-daemon-worktree-safe",
        );
        let output = run_stdio(
            &(format!("{bad_run_request}\n{bad_workspace_request}") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("rejected stdio");
        assert!(output.contains("reason:run_id_rejected"));
        assert!(output.contains("reason:workspace_rejected"));
        assert!(!output.contains(bad_run_id));
        assert!(!output.contains(bad_workspace.to_string_lossy().as_ref()));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        assert!(
            !workspace
                .join(".opensks")
                .join("runtime")
                .join("worktrees")
                .exists()
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    fn read_integration_verification(
        workspace: &Path,
        run_id: &str,
    ) -> opensks_contracts::IntegrationVerificationReceipt {
        serde_json::from_str(
            &std::fs::read_to_string(integration_artifact_path(
                workspace,
                run_id,
                "verification.json",
            ))
            .expect("verification receipt"),
        )
        .expect("verification json")
    }

    fn read_integration_cleanup(
        workspace: &Path,
        run_id: &str,
    ) -> opensks_contracts::IntegrationCleanupReceipt {
        serde_json::from_str(
            &std::fs::read_to_string(integration_artifact_path(workspace, run_id, "cleanup.json"))
                .expect("cleanup receipt"),
        )
        .expect("cleanup json")
    }

    fn integration_approval_request(run_id: &str) -> EngineRequest {
        let approval_id = format!("approval-integration-{run_id}");
        let mut request = EngineRequest::approval_decision(
            format!("req-approve-{run_id}"),
            run_id,
            approval_id,
            true,
        );
        request.params.scope = Some("integration_apply".to_string());
        request
    }

    fn spawn_chat_completion_server(path: &str, value: &str) -> (String, Arc<AtomicUsize>) {
        spawn_chat_completion_server_with_role_requests(path, value, 5)
    }

    fn spawn_chat_completion_server_with_role_requests(
        path: &str,
        value: &str,
        role_request_count: usize,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind chat server");
        let address = listener.local_addr().expect("local addr");
        let path = path.to_string();
        let value = value.to_string();
        let active_role_calls = Arc::new(AtomicUsize::new(0));
        let max_active_role_calls = Arc::new(AtomicUsize::new(0));
        let active_role_calls_for_thread = Arc::clone(&active_role_calls);
        let max_active_role_calls_for_thread = Arc::clone(&max_active_role_calls);
        thread::spawn(move || {
            let mut agent_index = 0;
            while agent_index < 2 {
                let (mut stream, _) = listener.accept().expect("accept chat request");
                let request = read_http_request(&mut stream);
                assert!(request.starts_with("POST /v1/chat/completions "));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer fixture-credential")
                );
                assert!(request.contains("\"model\":\"code-model\""));
                if request.contains("OpenSKS objective planner") {
                    let body = serde_json::json!({
                        "choices": [{
                            "message": {
                                "role": "assistant",
                                "content": "{\"max_parallelism\":5,\"role_count\":5,\"include_image_lane\":false,\"include_research_lane\":false}"
                            }
                        }]
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("write planner response");
                    continue;
                }
                let body = match agent_index {
                    0 => {
                        let arguments = serde_json::json!({
                            "path": path,
                            "value": value
                        });
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "tool_calls": [{
                                        "id": "call_1",
                                        "type": "function",
                                        "function": {
                                            "name": "append_line",
                                            "arguments": arguments.to_string()
                                        }
                                    }]
                                }
                            }]
                        })
                    }
                    1 => {
                        assert!(request.contains("wrote ROUTED_NOTE.md: applied=true"));
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": "Provider completed the edit."
                                }
                            }]
                        })
                    }
                    _ => unreachable!(),
                };
                agent_index += 1;
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write chat response");
            }
            let mut role_handles = Vec::new();
            for index in 2..(2 + role_request_count) {
                let (mut stream, _) = listener.accept().expect("accept role chat request");
                let active_role_calls = Arc::clone(&active_role_calls_for_thread);
                let max_active_role_calls = Arc::clone(&max_active_role_calls_for_thread);
                role_handles.push(thread::spawn(move || {
                    let request = read_http_request(&mut stream);
                    assert!(request.starts_with("POST /v1/chat/completions "));
                    assert!(
                        request
                            .to_ascii_lowercase()
                            .contains("authorization: bearer fixture-credential")
                    );
                    assert!(request.contains("\"model\":\"code-model\""));
                    assert!(
                        request.contains("bounded OpenSKS role worker")
                            || request.contains("isolated OpenSKS Code role subcontract worker")
                    );
                    assert!(request.contains(
                        "Worker context pack ref: artifact://.opensks/wiki/context-packs/generated/"
                    ));
                    assert!(
                        request.contains("--worker-turn-role-")
                            || request.contains("--worker-work-template-")
                    );
                    assert!(request.contains("Worker context:"));
                    assert!(request.contains("## Worker Scope"));
                    assert!(
                        request.contains("work_item_id: turn-role-")
                            || request.contains("work_item_id: work-template-")
                    );
                    let active_now = active_role_calls.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active_role_calls.fetch_max(active_now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(80));
                    let body = if request.contains("Role: code")
                        && !request.contains("wrote ROLE_CODE_NOTE.md: applied=true")
                    {
                        let arguments = serde_json::json!({
                            "path": "ROLE_CODE_NOTE.md",
                            "value": "code role candidate"
                        });
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "tool_calls": [{
                                        "id": "call_role_code",
                                        "type": "function",
                                        "function": {
                                            "name": "append_line",
                                            "arguments": arguments.to_string()
                                        }
                                    }]
                                }
                            }]
                        })
                    } else if request.contains("wrote ROLE_CODE_NOTE.md: applied=true") {
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": "Code role candidate patch is ready."
                                }
                            }]
                        })
                    } else if request.contains("Role: verification") {
                        assert!(request.contains("VERDICT: pass"));
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": "VERDICT: pass\n- Candidate is semantically aligned."
                                }
                            }]
                        })
                    } else {
                        assert!(request.contains("Return at most three short bullets."));
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": format!("Role subcall {index} assessed readiness.")
                                }
                            }]
                        })
                    };
                    let body = body.to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("write role chat response");
                    active_role_calls.fetch_sub(1, Ordering::SeqCst);
                }));
            }
            for handle in role_handles {
                handle.join().expect("role chat handler");
            }
        });
        (format!("http://{address}/v1"), max_active_role_calls)
    }

    fn spawn_image_generation_server() -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind image server");
        let address = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept image request");
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /v1/images/generations "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-credential")
            );
            assert!(request.contains("\"model\":\"gpt-image-1.5\""));
            assert!(request.contains("\"prompt\":\"render daemon image\""));
            assert!(request.contains("\"size\":\"64x64\""));
            let body = serde_json::json!({
                "created": 1,
                "data": [{
                    "b64_json": "iVBORw0KGgpvcGVuc2tzLWRhZW1vbi1pbWFnZQ=="
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write image response");
            request
        });
        (format!("http://{address}/v1"), handle)
    }

    fn spawn_vision_completion_server() -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind vision server");
        let address = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept vision request");
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /v1/chat/completions "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer fixture-credential")
            );
            assert!(request.contains("\"model\":\"gpt-vision-1.5\""));
            assert!(request.contains("Describe this generated image"));
            assert!(request.contains("\"type\":\"image_url\""));
            assert!(request.contains("data:image/x-portable-pixmap;base64,"));
            let body = serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "The image is a generated daemon fixture."
                    }
                }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write vision response");
            request
        });
        (format!("http://{address}/v1"), handle)
    }

    fn spawn_provider_image_tool_conversation_server() -> (String, thread::JoinHandle<Vec<String>>)
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider tool server");
        let address = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for step in 0..9 {
                let (mut stream, _) = listener.accept().expect("accept provider tool request");
                let request = read_http_request(&mut stream);
                let body = match step {
                    0 => {
                        assert!(request.starts_with("POST /v1/chat/completions "));
                        assert!(
                            request
                                .to_ascii_lowercase()
                                .contains("authorization: bearer fixture-credential")
                        );
                        assert!(request.contains("\"model\":\"code-model\""));
                        assert!(request.contains("image__generate"));
                        assert!(request.contains("image__inspect"));
                        let arguments = serde_json::json!({
                            "prompt": "render a provider E2E image",
                            "asset_id": "agent-e2e-image",
                            "width": 64,
                            "height": 64
                        });
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "tool_calls": [{
                                        "id": "call_generate",
                                        "type": "function",
                                        "function": {
                                            "name": "image__generate",
                                            "arguments": arguments.to_string()
                                        }
                                    }]
                                }
                            }]
                        })
                    }
                    1 => {
                        assert!(request.starts_with("POST /v1/images/generations "));
                        assert!(
                            request
                                .to_ascii_lowercase()
                                .contains("authorization: bearer fixture-credential")
                        );
                        assert!(request.contains("\"model\":\"gpt-image-selected\""));
                        assert!(!request.contains("\"model\":\"gpt-image-auto\""));
                        assert!(request.contains("\"prompt\":\"render a provider E2E image\""));
                        assert!(request.contains("\"size\":\"64x64\""));
                        serde_json::json!({
                            "created": 1,
                            "data": [{
                                "b64_json": "iVBORw0KGgpvcGVuc2tzLXByb3ZpZGVyLWUyZS1pbWFnZQ=="
                            }]
                        })
                    }
                    2 => {
                        assert!(request.starts_with("POST /v1/chat/completions "));
                        assert!(request.contains("\"model\":\"code-model\""));
                        assert!(request.contains("agent-e2e-image"));
                        assert!(request.contains("image.generate"));
                        let arguments = serde_json::json!({
                            "artifact_ref": "agent-e2e-image",
                            "prompt": "Describe generated provider fixture"
                        });
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "tool_calls": [{
                                        "id": "call_inspect",
                                        "type": "function",
                                        "function": {
                                            "name": "image__inspect",
                                            "arguments": arguments.to_string()
                                        }
                                    }]
                                }
                            }]
                        })
                    }
                    3 => {
                        assert!(request.starts_with("POST /v1/chat/completions "));
                        assert!(
                            request
                                .to_ascii_lowercase()
                                .contains("authorization: bearer fixture-credential")
                        );
                        assert!(request.contains("\"model\":\"gpt-vision-1.5\""));
                        assert!(request.contains("Describe generated provider fixture"));
                        assert!(request.contains("\"type\":\"image_url\""));
                        assert!(request.contains("data:image/png;base64,"));
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": "The generated provider fixture is visible."
                                }
                            }]
                        })
                    }
                    4 => {
                        assert!(request.starts_with("POST /v1/chat/completions "));
                        assert!(request.contains("\"model\":\"code-model\""));
                        assert!(request.contains("The generated provider fixture is visible."));
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": "Image generation and inspection receipts are complete."
                                }
                            }]
                        })
                    }
                    _ => {
                        assert!(request.starts_with("POST /v1/chat/completions "));
                        assert!(
                            request.contains("\"model\":\"code-model\"")
                                || request.contains("\"model\":\"gpt-vision-1.5\"")
                        );
                        assert!(
                            request.contains("bounded OpenSKS role worker")
                                || request.contains("isolated OpenSKS Code role subcontract worker")
                        );
                        let content = if request.contains("Role: verification") {
                            assert!(request.contains("VERDICT: pass"));
                            format!("VERDICT: pass\n- Image role subcall {step} assessed readiness.")
                        } else {
                            format!("Image role subcall {step} assessed readiness.")
                        };
                        serde_json::json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": content
                                }
                            }]
                        })
                    }
                }
                .to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write provider tool response");
                requests.push(request);
            }
            requests
        });
        (format!("http://{address}/v1"), handle)
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let bytes = stream.read(&mut chunk).expect("read http request");
            if bytes == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..bytes]);
            if let Some(total_len) = expected_http_request_len(&buffer)
                && buffer.len() >= total_len
            {
                break;
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    fn expected_http_request_len(buffer: &[u8]) -> Option<usize> {
        let header_end = buffer.windows(4).position(|window| window == b"\r\n\r\n")?;
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let content_len = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })?;
        Some(header_end + 4 + content_len)
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
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            repo.upsert_summary(
                &conversation_id,
                "User asked to preserve selected context for the planner.",
                3,
                1_700,
            )
            .expect("conversation summary");
        }
        std::fs::write(workspace.join("NOTE.md"), "selected context\nunselected\n")
            .expect("selected context fixture");
        let selected_hash = fnv1a64("selected context".as_bytes());
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-turn-start",
            "idem-conversation-turn-start",
        );
        turn_request.context.refs = vec![
            format!("editor://NOTE.md#L1-L1#{selected_hash}"),
            "symbol://conversation_root".to_string(),
        ];
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

        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted.run_id).expect("replay events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::WorkItemQueued);
        assert_eq!(
            events[0].payload["work_item_id"],
            opensks_scheduler::conversation_turn_root_work_item_id(&accepted.turn_id)
        );
        assert_eq!(events[0].payload["to"], "Ready");
        assert_eq!(events[0].payload["max_parallelism"], 4);
        assert_eq!(events[0].payload["work_item"]["kind"], "planning");
        let context_pack_ref = events[0].payload["work_item"]["context_pack_ref"]
            .as_str()
            .expect("work item context pack ref");
        assert!(context_pack_ref.starts_with("artifact://.opensks/wiki/context-packs/generated/"));
        assert!(context_pack_ref.ends_with(".json"));
        assert_eq!(events[0].payload["context_pack_ref"], context_pack_ref);
        assert!(
            events[0]
                .evidence_refs
                .iter()
                .any(|reference| reference == "context:turn-context-pack")
        );
        let context_pack_path = workspace.join(context_pack_ref.trim_start_matches("artifact://"));
        assert!(
            context_pack_path.exists(),
            "context pack artifact is written"
        );
        let context_pack_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&context_pack_path).expect("context pack json"),
        )
        .expect("context pack artifact json");
        assert_eq!(context_pack_json["schema"], "opensks.context-pack.v1");
        assert_eq!(
            context_pack_json["freshness"]["schema"],
            "opensks.intel-freshness.v1"
        );
        assert_eq!(
            context_pack_json["turn_context_refs"][0],
            format!("editor://NOTE.md#L1-L1#{selected_hash}")
        );
        assert_eq!(
            context_pack_json["turn_context_refs"][1],
            "symbol://conversation_root"
        );
        assert!(
            context_pack_json["body"]
                .as_str()
                .expect("context body")
                .contains("## Turn Context Refs")
        );
        assert!(
            context_pack_json["body"]
                .as_str()
                .expect("context body")
                .contains("## Freshness")
        );
        assert!(
            context_pack_json["turn_context_items"][0]["resolved"]
                .as_bool()
                .expect("resolved")
        );
        assert_eq!(
            context_pack_json["turn_context_items"][0]["reason_code"],
            "fresh"
        );
        assert_eq!(
            context_pack_json["turn_context_items"][0]["body"],
            "selected context"
        );
        assert_eq!(
            context_pack_json["conversation_summary"]["conversation_id"],
            conversation_id
        );
        assert_eq!(
            context_pack_json["conversation_summary"]["source_message_sequence"],
            3
        );
        assert_eq!(
            context_pack_json["conversation_summary"]["reason_code"],
            "redacted_conversation_summary"
        );
        assert!(
            context_pack_json["body"]
                .as_str()
                .expect("context body")
                .contains("## Conversation Summary")
        );
        assert!(
            context_pack_json["body"]
                .as_str()
                .expect("context body")
                .contains("preserve selected context")
        );
        assert!(!output.contains(context_pack_path.to_string_lossy().as_ref()));
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
        assert_eq!(ticks[0]["claimed"]["fencing_token"], 1);
        assert_eq!(ticks[0]["executed"]["status"], "executed", "{output}");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");
        assert_eq!(ticks[0]["executed"]["lease_heartbeat"]["started"], true);
        assert_eq!(
            ticks[0]["executed"]["lease_heartbeat"]["initial_heartbeat_ok"],
            true
        );
        assert_eq!(ticks[0]["executed"]["lease_heartbeat"]["fencing_token"], 1);
        assert_eq!(ticks[0]["executed"]["lease_heartbeat"]["interval_ms"], 333);
        assert!(
            ticks[0]["executed"]["last_event_sequence"]
                .as_u64()
                .unwrap_or(0)
                >= 4
        );
        assert_eq!(ticks[0]["executed"]["execution_workspace_mode"], "snapshot");
        assert_eq!(ticks[0]["executed"]["execution_isolated"], true);
        assert_eq!(
            ticks[0]["executed"]["execution_isolation_reason_code"],
            "snapshot_isolation_for_non_git_workspace"
        );
        assert_eq!(ticks[0]["executed"]["integration_state"], "candidate_ready");
        assert_eq!(ticks[0]["executed"]["integration_target_count"], 1);
        assert_eq!(
            ticks[0]["executed"]["integration_planned_verifier_count"],
            1
        );
        assert_eq!(
            ticks[0]["executed"]["integration_candidate_ref"],
            format!(
                "artifact://.opensks/runtime/integration-candidates/{}/candidate.json",
                accepted[0].run_id
            )
        );
        assert_eq!(
            ticks[0]["executed"]["integration_selection_ref"],
            format!(
                "artifact://.opensks/runtime/integration-candidates/{}/selection.json",
                accepted[0].run_id
            )
        );

        assert!(
            !workspace.join("SUPERVISOR_NOTE.md").exists(),
            "worktree-mode execution must not write into the user workspace"
        );
        let isolated_workspace = workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(&accepted[0].run_id)
            .join("turn-supervisor");
        let written = std::fs::read_to_string(isolated_workspace.join("SUPERVISOR_NOTE.md"))
            .expect("isolated written file");
        assert_eq!(written, "written by daemon supervisor");
        let candidate_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(&accepted[0].run_id);
        let candidate: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(candidate_dir.join("candidate.json"))
                .expect("candidate receipt"),
        )
        .expect("candidate json");
        assert_eq!(candidate["schema"], "opensks.integration-candidate.v1");
        assert_eq!(candidate["state"], "candidate_ready");
        assert_eq!(candidate["target_paths"][0], "SUPERVISOR_NOTE.md");
        assert_eq!(candidate["main_workspace_modified"], false);
        assert_eq!(candidate["approval_required"], true);
        assert_eq!(candidate["content_redacted"], true);
        assert_eq!(
            candidate["selection_ref"],
            format!(
                "artifact://.opensks/runtime/integration-candidates/{}/selection.json",
                accepted[0].run_id
            )
        );
        let selection: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(candidate_dir.join("selection.json"))
                .expect("selection receipt"),
        )
        .expect("selection json");
        assert_eq!(
            selection["schema"],
            "opensks.integration-candidate-selection-receipt.v1"
        );
        assert_eq!(selection["selected_candidate_id"], candidate["id"]);
        assert_eq!(
            selection["selection_policy"],
            "deterministic_single_ready_candidate"
        );
        assert_eq!(selection["aggregate_candidate_count"], 1);
        assert_eq!(selection["aggregate_target_count"], 1);
        assert_eq!(selection["planned_verifier_count"], 1);
        assert_eq!(
            selection["selected_source_candidate_ids"][0],
            format!("turn-supervisor-candidate-{}", accepted[0].run_id)
        );
        assert_eq!(
            selection["required_verification_gates"][0],
            "candidate_receipt_valid"
        );
        let patch = std::fs::read_to_string(candidate_dir.join("candidate.patch"))
            .expect("candidate patch");
        assert!(patch.contains("SUPERVISOR_NOTE.md"));
        assert!(patch.contains("+written by daemon supervisor"));
        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert_eq!(messages[1].state, MessageState::Complete);
        assert!(messages[1].content_redacted.contains("SUPERVISOR_NOTE.md"));
        assert!(
            messages[1]
                .content_redacted
                .contains("integration is pending")
        );
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
        let scheduler_root_id =
            opensks_scheduler::conversation_turn_root_work_item_id(&accepted[0].turn_id);
        let scheduler_completed = events
            .iter()
            .find(|event| {
                event.kind == EventKind::WorkItemCompleted
                    && event.payload["work_item_id"] == scheduler_root_id
                    && event
                        .evidence_refs
                        .iter()
                        .any(|evidence| evidence == "daemon:turn-scheduler-worker-route")
            })
            .expect("scheduler worker completion event");
        assert_eq!(
            scheduler_completed.payload["lease_holder"],
            "turn-supervisor"
        );
        assert_eq!(scheduler_completed.payload["worker_id"], "turn-supervisor");
        assert_eq!(scheduler_completed.payload["to"], "Completed");
        assert!(
            scheduler_completed
                .evidence_refs
                .iter()
                .any(|evidence| evidence == "scheduler:lease-visible-to-worker")
        );
        assert!(
            scheduler_completed
                .evidence_refs
                .iter()
                .any(|evidence| evidence == "adapter:request-patch-lease")
        );
        assert!(
            scheduler_completed
                .evidence_refs
                .iter()
                .any(|evidence| evidence == "patch-engine:fence-token-bound")
        );
        let scheduler_lease_id = scheduler_completed.payload["lease_id"]
            .as_str()
            .expect("scheduler lease id");
        let patch_journal_dir = isolated_workspace
            .join(".opensks")
            .join("patch-engine")
            .join("transactions");
        let patch_journal = std::fs::read_dir(&patch_journal_dir)
            .expect("patch journal dir")
            .map(|entry| std::fs::read_to_string(entry.expect("journal entry").path()).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(patch_journal.contains("\"raw_tokens_redacted\":true"));
        assert!(
            !patch_journal.contains(scheduler_lease_id),
            "patch journal must hash/redact the scheduler lease fence"
        );
        assert_eq!(
            events.first().map(|event| &event.kind),
            Some(&EventKind::WorkItemQueued)
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::RunStarted),
            "supervisor execution must append run start after scheduler bootstrap: {events:#?}"
        );
        assert!(
            events.iter().any(|event| {
                event.kind == EventKind::RunCompleted
                    && event.payload["state"] == "completed"
                    && event
                        .evidence_refs
                        .iter()
                        .any(|evidence| evidence == "conversation:run-completed")
            }),
            "successful supervisor execution must append terminal run completion: {events:#?}"
        );
        assert!(
            events.iter().any(|event| {
                event.payload["agent_event_kind"] == "file_patch_applied"
                    && event.kind == EventKind::WorkItemCompleted
            }),
            "supervisor execution must persist patch events: {events:#?}"
        );
        assert!(
            events.iter().any(|event| {
                event.payload["agent_event_kind"] == "worker_spawned"
                    && event.payload["payload"]["code"] == "execution_workspace_prepared"
                    && event.payload["payload"]["isolation_mode"] == "snapshot"
                    && event.payload["payload"]["path_redacted"] == true
            }),
            "supervisor execution must persist redacted isolation evidence: {events:#?}"
        );
        assert!(
            events.iter().any(|event| {
                event.payload["agent_event_kind"] == "worker_progress"
                    && event.payload["payload"]["code"] == "integration_candidate_ready"
                    && event.payload["payload"]["receipt_ref"] == candidate["receipt_ref"]
                    && event.payload["payload"]["main_workspace_modified"] == false
            }),
            "supervisor execution must persist integration candidate evidence: {events:#?}"
        );
        assert!(
            !serde_json::to_string(&events)
                .expect("events json")
                .contains(workspace.to_string_lossy().as_ref()),
            "runtime event journal must not expose the absolute user workspace path"
        );

        repo.upsert_run_projection_with_last_sequence(
            &accepted[0].run_id,
            &project_id,
            &conversation_id,
            &accepted[0].turn_id,
            "failed",
            99,
            2_900,
        )
        .expect("stale projection");
        assert_eq!(
            repo.run_projection_state(&accepted[0].run_id)
                .expect("stale run state")
                .as_deref(),
            Some("failed")
        );
        let subscribe = EngineRequest::subscribe_events(
            "req-supervisor-completed-rebuild",
            &accepted[0].run_id,
            Some(0),
        );
        run_stdio(
            &(serde_json::to_string(&subscribe).expect("subscribe request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("subscribe replay stdio");
        let rebuilt_repo =
            ConversationRepository::open_workspace(&workspace).expect("rebuilt repo");
        assert_eq!(
            rebuilt_repo
                .run_projection_state(&accepted[0].run_id)
                .expect("rebuilt run state")
                .as_deref(),
            Some("completed"),
            "cursor-0 replay must derive completed state from run_completed"
        );
        assert_eq!(
            rebuilt_repo
                .get_conversation(&conversation_id)
                .expect("conversation")
                .expect("conversation present")
                .status,
            ConversationStatus::Completed
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_blocks_redacted_prompt_without_raw_vault() {
        let workspace = temp_workspace("conversation-supervisor-redacted-prompt");
        disable_test_raw_prompt_identity_provisioning(&workspace);
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-redacted-prompt-start",
            "idem-conversation-redacted-prompt-start",
        );
        turn_request.message.text = local_test_secret_write_prompt(
            "REDACTED_PROMPT_NOTE.md",
            "sk-daemon-redacted-prompt-secret",
        );
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-redacted-prompt-tick",
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
        assert_eq!(ticks[0]["executed"]["status"], "executed", "{output}");
        assert_eq!(ticks[0]["executed"]["run_state"], "failed");
        assert!(
            !workspace.join("REDACTED_PROMPT_NOTE.md").exists(),
            "redacted prompt must not be executed as a local-test write"
        );

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert!(messages[0].content_redacted.contains("[REDACTED]"));
        assert_eq!(messages[1].state, MessageState::Failed);
        assert!(
            messages[1]
                .content_redacted
                .contains("raw prompt vault is unavailable")
        );
        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted[0].run_id).expect("replay events");
        assert!(
            events.iter().any(|event| {
                event.kind == EventKind::VerificationFailed
                    && event.payload["agent_event_kind"] == "error"
                    && event.payload["payload"]["code"] == "raw_prompt_unavailable_after_redaction"
                    && event
                        .evidence_refs
                        .iter()
                        .any(|reference| reference == "conversation:redacted-prompt-fail-closed")
            }),
            "raw prompt fail-closed event must be durable: {events:#?}"
        );
        assert!(
            !serde_json::to_string(&events)
                .expect("events json")
                .contains("sk-daemon-redacted"),
            "secret-like raw prompt content must not leak into events: {events:#?}"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_restores_encrypted_raw_prompt_for_execution() {
        let workspace = temp_workspace("conversation-supervisor-encrypted-raw-prompt");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let raw_prompt = local_test_secret_write_prompt(
            "RESTORED_RAW_PROMPT_NOTE.md",
            "sk-daemon-restored-prompt-secret",
        );
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        let ciphertext =
            opensks_vault::encrypt_bytes(raw_prompt.as_bytes(), &recipient).expect("encrypt raw");
        let secret_identity = identity.to_string();
        install_test_raw_prompt_identity_text(
            &workspace,
            age::secrecy::ExposeSecret::expose_secret(&secret_identity).to_string(),
        );

        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-encrypted-raw-prompt-start",
            "idem-conversation-encrypted-raw-prompt-start",
        );
        turn_request.message.text = raw_prompt.clone();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let start_output = run_stdio(
            &(serde_json::to_string(&start).expect("start request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("start stdio");
        let accepted = accepted_lines(&start_output);
        assert_eq!(accepted.len(), 1);

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        repo.set_message_content_with_raw_ciphertext(
            &accepted[0].user_message_id,
            &raw_prompt,
            MessageState::Complete,
            Some(&opensks_conversation::MessageRawContentCiphertext {
                ciphertext,
                nonce: b"age-x25519-v1".to_vec(),
            }),
            2_100,
        )
        .expect("attach encrypted raw prompt");

        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-encrypted-raw-prompt-tick",
            "daemon-test-supervisor",
            1_000,
        );
        let output = run_stdio(
            &(serde_json::to_string(&tick).expect("tick request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("tick stdio");
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["executed"]["status"], "executed", "{output}");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");
        let isolated_workspace = workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(&accepted[0].run_id)
            .join("turn-supervisor");
        assert_eq!(
            std::fs::read_to_string(isolated_workspace.join("RESTORED_RAW_PROMPT_NOTE.md"))
                .expect("restored raw prompt write"),
            test_openai_key_assignment("sk-daemon-restored-prompt-secret")
        );

        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert!(messages[0].content_redacted.contains("[REDACTED]"));
        assert!(!messages[0].content_redacted.contains("sk-daemon-restored"));
        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted[0].run_id).expect("replay events");
        assert!(
            events.iter().any(|event| {
                event.kind == EventKind::WorkItemRunning
                    && event.payload["agent_event_kind"] == "warning"
                    && event.payload["payload"]["code"] == "raw_prompt_restored_after_redaction"
                    && event
                        .evidence_refs
                        .iter()
                        .any(|reference| reference == "conversation:raw-prompt-vault-decrypt")
            }),
            "raw prompt restore event must be durable: {events:#?}"
        );
        assert!(
            !serde_json::to_string(&events)
                .expect("events json")
                .contains("sk-daemon-restored"),
            "secret-like raw prompt content must not leak into events: {events:#?}"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_turn_start_encrypts_raw_prompt_for_later_supervisor_restore() {
        let workspace = temp_workspace("conversation-turn-start-encrypts-raw-prompt");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let raw_prompt = local_test_secret_write_prompt(
            "PRODUCED_RAW_PROMPT_NOTE.md",
            "sk-daemon-produced-prompt-secret",
        );
        assert!(
            test_raw_prompt_identity_text(&workspace).is_none(),
            "test starts without a pre-provisioned raw prompt identity"
        );

        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-produced-raw-prompt-start",
            "idem-conversation-produced-raw-prompt-start",
        );
        turn_request.message.text = raw_prompt.clone();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-produced-raw-prompt-tick",
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
        assert!(
            test_raw_prompt_identity_text(&workspace).is_some(),
            "turn-start should provision a workspace raw prompt identity"
        );
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["executed"]["status"], "executed", "{output}");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let raw = repo
            .turn_user_message_raw_content_ciphertext(&accepted[0].turn_id)
            .expect("raw ciphertext")
            .expect("turn-start should store encrypted raw prompt");
        assert!(!raw.ciphertext.is_empty());
        assert_eq!(raw.nonce, b"age-x25519-keychain-v1");
        assert!(
            !String::from_utf8_lossy(&raw.ciphertext).contains("sk-daemon-produced"),
            "ciphertext must not contain raw secret-like token"
        );
        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert!(messages[0].content_redacted.contains("[REDACTED]"));
        assert!(!messages[0].content_redacted.contains("sk-daemon-produced"));

        let isolated_workspace = workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(&accepted[0].run_id)
            .join("turn-supervisor");
        assert_eq!(
            std::fs::read_to_string(isolated_workspace.join("PRODUCED_RAW_PROMPT_NOTE.md"))
                .expect("produced raw prompt write"),
            test_openai_key_assignment("sk-daemon-produced-prompt-secret")
        );
        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted[0].run_id).expect("replay events");
        assert!(
            events.iter().any(|event| {
                event.payload["payload"]["code"] == "raw_prompt_restored_after_redaction"
                    && event
                        .evidence_refs
                        .iter()
                        .any(|reference| reference == "conversation:raw-prompt-vault-decrypt")
            }),
            "raw prompt restore event must be durable: {events:#?}"
        );
        assert!(
            !serde_json::to_string(&events)
                .expect("events json")
                .contains("sk-daemon-produced"),
            "secret-like raw prompt content must not leak into events: {events:#?}"
        );
        assert!(!output.contains("sk-daemon-produced"));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_candidate_ready_isolation_recovers_after_abandoned_turn() {
        let workspace = temp_workspace("conversation-supervisor-recovery-e2e");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-supervisor-recovery-start",
            "idem-conversation-supervisor-recovery-start",
        );
        turn_request.message.text = r#"{"local_test":{"op":"create_file","path":"RECOVERABLE_SUPERVISOR_NOTE.md","value":"written before abandoned cleanup"}}"#.to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-supervisor-recovery-tick",
            "daemon-recovery-supervisor",
            1_000,
        );
        let output = run_stdio(
            &([
                serde_json::to_string(&start).expect("start request"),
                serde_json::to_string(&tick).expect("tick request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("supervisor stdio");

        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["executed"]["integration_state"], "candidate_ready");
        assert_eq!(
            ticks[0]["executed"]["execution_isolation_reason_code"],
            "snapshot_isolation_for_non_git_workspace"
        );
        let run_id = &accepted[0].run_id;
        let isolated_workspace = source_isolation_path(&workspace, run_id, "turn-supervisor");
        assert!(
            isolated_workspace
                .join("RECOVERABLE_SUPERVISOR_NOTE.md")
                .exists(),
            "candidate-ready supervisor execution should leave a recoverable source isolation"
        );
        let secret = "SERVICE_TOKEN=sk-daemon-recovery-e2e-secret";
        std::fs::write(isolated_workspace.join(".env"), secret).expect("secret recovery fixture");

        let recovery_request = worktree_isolation_request(
            WORKTREE_ISOLATION_RECOVERY_KIND,
            "req-conversation-supervisor-recovery-cleanup",
            &workspace,
            run_id,
        );
        let recovery_output = run_stdio(
            &(recovery_request + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("recovery stdio");

        assert!(recovery_output.contains("opensks.worktree-isolation-inventory-receipt.v1"));
        assert!(recovery_output.contains("opensks.worktree-isolation-recovery-receipt.v1"));
        assert!(recovery_output.contains("\"daemon:worktree-isolation-recovery\""));
        assert!(!recovery_output.contains(workspace.to_string_lossy().as_ref()));
        assert!(!recovery_output.contains(secret));
        assert!(
            !isolated_workspace.exists(),
            "daemon recovery request should remove the abandoned turn-supervisor isolation"
        );

        let recovery_raw = std::fs::read_to_string(worktree_isolation_artifact_path(
            &workspace,
            run_id,
            "recovery.json",
        ))
        .expect("recovery receipt");
        assert!(!recovery_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!recovery_raw.contains(secret));
        let recovery: opensks_contracts::WorktreeIsolationRecoveryReceipt =
            serde_json::from_str(&recovery_raw).expect("recovery json");
        assert_eq!(recovery.run_id, run_id.as_str());
        assert_eq!(recovery.state, "recovered");
        assert_eq!(recovery.target_count, 1);
        assert_eq!(recovery.recovered_count, 1);
        assert!(recovery.path_redacted);
        assert!(recovery.content_redacted);
        assert_eq!(recovery.targets[0].worker_id, "turn-supervisor");
        assert!(
            integration_artifact_path(&workspace, run_id, "candidate.json").exists(),
            "cleanup should remove the source isolation without deleting the candidate receipt"
        );

        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(run_id)
            .expect("replay events");
        let events_raw = serde_json::to_string(&events).expect("events json");
        assert!(events_raw.contains("integration_candidate_ready"));
        assert!(events_raw.contains("daemon.worktree_isolation_recovery"));
        assert!(!events_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!events_raw.contains(secret));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_recovers_abandoned_candidate_ready_turn_without_reexecuting() {
        let workspace = temp_workspace("conversation-supervisor-auto-recovery");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-supervisor-auto-recovery-start",
            "idem-conversation-supervisor-auto-recovery-start",
        );
        turn_request.message.text = r#"{"local_test":{"op":"create_file","path":"AUTO_RECOVERY_NOTE.md","value":"written once before recovery"}}"#.to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let first_tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-supervisor-auto-recovery-first-tick",
            "daemon-first-supervisor",
            1_000,
        );
        let first_output = run_stdio(
            &([
                serde_json::to_string(&start).expect("start request"),
                serde_json::to_string(&first_tick).expect("first tick request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("first supervisor stdio");
        let accepted = accepted_lines(&first_output);
        assert_eq!(accepted.len(), 1);
        let run_id = accepted[0].run_id.clone();
        let turn_id = accepted[0].turn_id.clone();
        let first_ticks = supervisor_tick_lines(&first_output);
        assert_eq!(first_ticks.len(), 1);
        assert_eq!(
            first_ticks[0]["executed"]["integration_state"],
            "candidate_ready"
        );
        let isolated_workspace = source_isolation_path(&workspace, &run_id, "turn-supervisor");
        assert!(isolated_workspace.join("AUTO_RECOVERY_NOTE.md").exists());
        let secret = "SERVICE_TOKEN=sk-auto-recovery-secret";
        std::fs::write(isolated_workspace.join(".env"), secret).expect("secret fixture");
        assert!(integration_artifact_path(&workspace, &run_id, "candidate.json").exists());
        assert!(integration_artifact_path(&workspace, &run_id, "candidate.patch").exists());
        assert!(integration_artifact_path(&workspace, &run_id, "selection.json").exists());
        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events_before = store.replay(&run_id).expect("events before");
        let file_patch_events_before = events_before
            .iter()
            .filter(|event| {
                event.kind == EventKind::WorkItemCompleted
                    && event.payload["agent_event_kind"] == "file_patch_applied"
            })
            .count();
        assert_eq!(file_patch_events_before, 1);

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        repo.set_turn_run_state(&turn_id, &run_id, "queued", 50_000)
            .expect("requeue completed run as abandoned fixture");
        let stale_lease = repo
            .claim_next_queued_turn("crashed-supervisor", 1, 1)
            .expect("claim stale fixture")
            .expect("stale lease fixture");
        assert_eq!(stale_lease.run_id, run_id);
        assert_eq!(
            repo.run_projection_state(&run_id)
                .expect("running state")
                .as_deref(),
            Some("running")
        );

        let recovery_tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-supervisor-auto-recovery-second-tick",
            "daemon-recovery-supervisor",
            1_000,
        );
        let recovery_output = run_stdio(
            &(serde_json::to_string(&recovery_tick).expect("recovery tick") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("recovery supervisor stdio");
        let recovery_ticks = supervisor_tick_lines(&recovery_output);
        assert_eq!(recovery_ticks.len(), 1);
        assert_eq!(recovery_ticks[0]["recovered_expired_leases"], 1);
        assert_eq!(recovery_ticks[0]["claimed"]["run_id"], run_id);
        assert_eq!(
            recovery_ticks[0]["executed"]["status"],
            "recovered_candidate_ready"
        );
        assert_eq!(recovery_ticks[0]["executed"]["execution_skipped"], true);
        assert_eq!(recovery_ticks[0]["executed"]["run_state"], "completed");
        assert_eq!(
            recovery_ticks[0]["executed"]["integration_state"],
            "candidate_ready"
        );
        assert_eq!(
            recovery_ticks[0]["executed"]["worktree_recovery_state"],
            "recovered"
        );
        assert_eq!(recovery_ticks[0]["executed"]["worktree_recovered_count"], 1);
        assert!(!recovery_output.contains(workspace.to_string_lossy().as_ref()));
        assert!(!recovery_output.contains(secret));
        assert!(!isolated_workspace.exists());
        assert!(
            !workspace.join("AUTO_RECOVERY_NOTE.md").exists(),
            "automatic recovery must not apply the candidate to the main workspace"
        );
        assert!(integration_artifact_path(&workspace, &run_id, "candidate.json").exists());
        assert!(integration_artifact_path(&workspace, &run_id, "candidate.patch").exists());
        assert!(integration_artifact_path(&workspace, &run_id, "selection.json").exists());

        let recovery_raw = std::fs::read_to_string(worktree_isolation_artifact_path(
            &workspace,
            &run_id,
            "recovery.json",
        ))
        .expect("recovery receipt");
        assert!(!recovery_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!recovery_raw.contains(secret));
        let recovery: opensks_contracts::WorktreeIsolationRecoveryReceipt =
            serde_json::from_str(&recovery_raw).expect("recovery json");
        assert_eq!(recovery.state, "recovered");
        assert_eq!(recovery.recovered_count, 1);
        assert_eq!(recovery.targets[0].worker_id, "turn-supervisor");

        let events_after = store.replay(&run_id).expect("events after");
        let file_patch_events_after = events_after
            .iter()
            .filter(|event| {
                event.kind == EventKind::WorkItemCompleted
                    && event.payload["agent_event_kind"] == "file_patch_applied"
            })
            .count();
        assert_eq!(
            file_patch_events_after, file_patch_events_before,
            "candidate-ready recovery must not re-run the patch tool"
        );
        let events_raw = serde_json::to_string(&events_after).expect("events json");
        assert!(events_raw.contains("daemon.worktree_isolation_recovery"));
        assert!(!events_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!events_raw.contains(secret));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_run_control_cancel_projects_before_supervisor_tick() {
        let workspace = temp_workspace("conversation-run-control-cancel-projects");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-run-control-cancel-start",
            "idem-conversation-run-control-cancel-start",
        );
        turn_request.message.text =
            r#"{"local_test":{"op":"create_file","path":"PROJECTED_CANCEL.md","value":"must not write"}}"#
                .to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let start_output = run_stdio(
            &(serde_json::to_string(&start).expect("start request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("start stdio");
        let accepted = accepted_lines(&start_output);
        assert_eq!(accepted.len(), 1);

        let cancel = EngineRequest::run_cancel(
            "req-conversation-run-control-cancel",
            accepted[0].run_id.clone(),
        );
        let cancel_output = run_stdio(
            &(serde_json::to_string(&cancel).expect("cancel request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("cancel stdio");
        assert!(cancel_output.contains("\"kind\":\"run_cancelled\""));
        assert!(cancel_output.contains("conversation:run-control-projected"));

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        assert_eq!(
            repo.run_projection_state(&accepted[0].run_id)
                .expect("run projection")
                .as_deref(),
            Some("cancelled")
        );
        let timeline = repo
            .timeline_items_for_conversation(&conversation_id, 20)
            .expect("timeline");
        assert!(
            timeline.iter().any(|item| {
                item.run_id.as_deref() == Some(accepted[0].run_id.as_str())
                    && item.id.starts_with("timeline-event-")
                    && item.state == "run_cancelled"
            }),
            "run_cancel must be projected as a durable timeline event: {timeline:#?}"
        );
        assert!(
            timeline.iter().any(|item| {
                item.run_id.as_deref() == Some(accepted[0].run_id.as_str())
                    && !item.id.starts_with("timeline-event-")
                    && item.state == "cancelled"
                    && item.payload["message_state"] == "streaming"
            }),
            "assistant timeline row must read the cancelled run projection: {timeline:#?}"
        );

        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-run-control-cancel-tick",
            "daemon-test-supervisor",
            1_000,
        );
        let tick_output = run_stdio(
            &(serde_json::to_string(&tick).expect("tick request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("tick stdio");
        let ticks = supervisor_tick_lines(&tick_output);
        assert_eq!(ticks.len(), 1);
        assert!(
            ticks[0]["claimed"].is_null(),
            "projected cancellation must not be claimed by the supervisor: {ticks:#?}"
        );
        assert!(ticks[0]["executed"].is_null());
        assert!(
            !workspace.join("PROJECTED_CANCEL.md").exists(),
            "projected cancelled turns must not write into the user workspace"
        );
        assert!(
            !workspace
                .join(".opensks")
                .join("runtime")
                .join("worktrees")
                .join(&accepted[0].run_id)
                .join("turn-supervisor")
                .exists(),
            "projected cancelled turns must not prepare an execution workspace"
        );

        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(&accepted[0].run_id)
            .expect("replay events");
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::RunCancelled),
            "cancel receipt must stay in the event journal"
        );
        assert!(
            events
                .iter()
                .all(|event| event.kind != EventKind::RunStarted),
            "projected cancellation must not emit run_started"
        );
        assert!(
            events
                .iter()
                .all(|event| event.kind != EventKind::RunCompleted),
            "projected cancellation must not emit run_completed"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_honors_cancel_before_dispatch() {
        let workspace = temp_workspace("conversation-supervisor-cancel-before-dispatch");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-supervisor-cancel-start",
            "idem-conversation-supervisor-cancel-start",
        );
        turn_request.message.text = r#"{"local_test":{"op":"create_file","path":"CANCELLED_NOTE.md","value":"must not write"}}"#.to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let start_output = run_stdio(
            &(serde_json::to_string(&start).expect("start request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("start stdio");
        let accepted = accepted_lines(&start_output);
        assert_eq!(accepted.len(), 1);

        let cancel_result = opensks_engine::append_run_control_event(
            &workspace,
            &accepted[0].run_id,
            EventKind::RunCancelled,
            None::<&str>,
            "run cancel requested",
            "cancelled_by_user",
        )
        .expect("append raw cancel event");
        assert_eq!(cancel_result.event.kind, EventKind::RunCancelled);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            assert_eq!(
                repo.run_projection_state(&accepted[0].run_id)
                    .expect("run projection")
                    .as_deref(),
                Some("queued"),
                "raw journal cancellation without projection should still be caught by dispatch"
            );
        }

        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-supervisor-cancel-tick",
            "daemon-test-supervisor",
            1_000,
        );
        let output = run_stdio(
            &(serde_json::to_string(&tick).expect("tick request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("tick stdio");

        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["claimed"]["run_id"], accepted[0].run_id);
        assert_eq!(ticks[0]["executed"]["status"], "cancelled");
        assert_eq!(ticks[0]["executed"]["run_state"], "cancelled");
        assert_eq!(ticks[0]["executed"]["execution_skipped"], true);
        assert_eq!(
            ticks[0]["executed"]["cancellation"]["reason_code"],
            "cancelled_by_user"
        );
        assert_eq!(ticks[0]["executed"]["patch_count"], 0);
        assert_eq!(ticks[0]["executed"]["apply_result_count"], 0);
        assert!(
            !workspace.join("CANCELLED_NOTE.md").exists(),
            "cancelled turns must not write into the user workspace"
        );
        assert!(
            !workspace
                .join(".opensks")
                .join("runtime")
                .join("worktrees")
                .join(&accepted[0].run_id)
                .join("turn-supervisor")
                .exists(),
            "cancelled turns must not prepare an execution workspace"
        );

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        assert_eq!(
            repo.run_projection_state(&accepted[0].run_id)
                .expect("run projection")
                .as_deref(),
            Some("cancelled")
        );
        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(&accepted[0].run_id)
            .expect("replay events");
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::RunCancelled),
            "cancel receipt must stay in the event journal"
        );
        assert!(
            events
                .iter()
                .all(|event| event.kind != EventKind::RunStarted),
            "supervisor must not emit run_started after a prior cancel"
        );
        assert!(
            events
                .iter()
                .all(|event| event.kind != EventKind::RunCompleted),
            "supervisor must not emit run_completed after a prior cancel"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_dispatches_provider_registry_route_through_chat_adapter() {
        let workspace = temp_workspace("conversation-supervisor-registry-route");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let (endpoint, max_active_role_calls) =
            spawn_chat_completion_server("ROUTED_NOTE.md", "registry route dispatched");
        seed_healthy_provider_model_with_endpoint(&workspace, &endpoint, true);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Pinned,
                    model_id: Some("provider-1/code-model".to_string()),
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Standard,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "registry-route-test".to_string(),
                max_parallelism: 8,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
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
            "req-conversation-supervisor-route-start",
            "idem-conversation-supervisor-route-start",
        );
        turn_request.message.text = "Append a dispatch note to ROUTED_NOTE.md".to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-supervisor-route-tick",
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
        assert!(
            max_active_role_calls.load(Ordering::SeqCst) >= 2,
            "provider role subcalls should overlap up to the per-model cap"
        );
        assert!(!output.contains("fixture-credential"));
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["executed"]["status"], "executed", "{output}");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");
        assert_eq!(ticks[0]["executed"]["model_routing_status"], "dispatched");
        assert_eq!(
            ticks[0]["executed"]["model_routing_reason_code"],
            "provider_request_dispatched"
        );
        assert_eq!(
            ticks[0]["executed"]["selected_model_id"],
            "provider-1/code-model"
        );

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let routing_raw = repo
            .turn_model_routing_decision_json(&accepted[0].turn_id)
            .expect("routing decision")
            .expect("routing decision snapshot");
        let routing: opensks_contracts::RoutingDecision =
            serde_json::from_str(&routing_raw).expect("routing json");
        assert_eq!(routing.status, opensks_contracts::RoutingStatus::Dispatched);
        assert_eq!(routing.reason_code, "provider_request_dispatched");
        assert_eq!(
            routing.selected_model_id.as_deref(),
            Some("provider-1/code-model")
        );
        let receipt = routing.route_receipt.expect("route receipt");
        assert_eq!(receipt.provider_id.as_deref(), Some("provider-1"));
        assert_eq!(receipt.model_id.as_deref(), Some("provider-1/code-model"));
        assert!(
            !workspace.join("ROUTED_NOTE.md").exists(),
            "provider worktree-mode tool writes must stay isolated"
        );
        let isolated_workspace = workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(&accepted[0].run_id)
            .join("turn-supervisor");
        let written = std::fs::read_to_string(isolated_workspace.join("ROUTED_NOTE.md"))
            .expect("isolated written file");
        assert_eq!(written, "registry route dispatched\n");
        let candidate_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(&accepted[0].run_id);
        let candidate: opensks_contracts::IntegrationCandidateReceipt = serde_json::from_str(
            &std::fs::read_to_string(candidate_dir.join("candidate.json"))
                .expect("candidate receipt"),
        )
        .expect("candidate json");
        assert_eq!(
            candidate.schema,
            opensks_contracts::INTEGRATION_CANDIDATE_RECEIPT_SCHEMA
        );
        assert_eq!(candidate.state, "candidate_ready");
        assert_eq!(
            candidate.reason_code,
            "aggregate_isolated_patch_candidate_ready"
        );
        assert!(!candidate.main_workspace_modified);
        assert_eq!(candidate.aggregate_candidate_count, 2);
        assert_eq!(candidate.aggregate_target_count, 2);
        assert_eq!(candidate.planned_verifier_count, 1);
        let selection_ref = format!(
            "artifact://.opensks/runtime/integration-candidates/{}/selection.json",
            accepted[0].run_id
        );
        assert_eq!(
            candidate.selection_ref.as_deref(),
            Some(selection_ref.as_str())
        );
        assert!(
            candidate
                .target_paths
                .contains(&"ROUTED_NOTE.md".to_string())
        );
        assert!(
            candidate
                .target_paths
                .contains(&"ROLE_CODE_NOTE.md".to_string())
        );
        assert!(candidate.source_candidates.iter().any(|source| {
            source.source == "turn_supervisor"
                && source.worker_id == "turn-supervisor"
                && source.target_paths.contains(&"ROUTED_NOTE.md".to_string())
        }));
        assert!(candidate.source_candidates.iter().any(|source| {
            source.source == "role_subcontract"
                && source.role.as_deref() == Some("code")
                && source
                    .target_paths
                    .contains(&"ROLE_CODE_NOTE.md".to_string())
        }));
        let selection: opensks_contracts::IntegrationCandidateSelectionReceipt =
            serde_json::from_str(
                &std::fs::read_to_string(candidate_dir.join("selection.json"))
                    .expect("selection receipt"),
            )
            .expect("selection json");
        assert_eq!(
            selection.schema,
            opensks_contracts::INTEGRATION_CANDIDATE_SELECTION_RECEIPT_SCHEMA
        );
        assert_eq!(selection.selected_candidate_id, candidate.id);
        assert_eq!(selection.selected_candidate_ref, candidate.receipt_ref);
        assert_eq!(selection.selected_patch_ref, candidate.patch_ref);
        assert_eq!(
            selection.selection_policy,
            "deterministic_all_ready_source_candidates"
        );
        assert_eq!(selection.reason_code, "aggregate_ready_sources_selected");
        assert_eq!(selection.aggregate_candidate_count, 2);
        assert_eq!(selection.aggregate_target_count, 2);
        assert_eq!(selection.planned_verifier_count, 1);
        assert_eq!(selection.candidate_pool.len(), 2);
        let selected_ids = selection
            .selected_source_candidate_ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let source_ids = candidate
            .source_candidates
            .iter()
            .map(|source| source.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(selected_ids, source_ids);
        assert!(
            selection
                .target_paths
                .contains(&"ROUTED_NOTE.md".to_string())
        );
        assert!(
            selection
                .target_paths
                .contains(&"ROLE_CODE_NOTE.md".to_string())
        );
        assert!(
            selection
                .required_verification_gates
                .contains(&"patch_apply_check".to_string())
        );
        assert!(
            selection
                .evidence_refs
                .contains(&"integration:candidate-selection-receipt".to_string())
        );
        let aggregate_patch = std::fs::read_to_string(candidate_dir.join("candidate.patch"))
            .expect("candidate patch");
        assert!(aggregate_patch.contains("registry route dispatched"));
        assert!(aggregate_patch.contains("code role candidate"));
        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(&accepted[0].run_id)
            .expect("replay events");
        let scheduler_root_id =
            opensks_scheduler::conversation_turn_root_work_item_id(&accepted[0].turn_id);
        let scheduler_queued = events
            .iter()
            .find(|event| {
                event.kind == EventKind::WorkItemQueued
                    && event.payload["work_item_id"] == scheduler_root_id
            })
            .expect("scheduler queued root");
        assert_eq!(scheduler_queued.payload["max_parallelism"], 8);
        assert_eq!(scheduler_queued.payload["provider_max_workers"], 2);
        assert_eq!(scheduler_queued.payload["per_provider_max_workers"], 2);
        assert_eq!(scheduler_queued.payload["per_model_max_workers"], 2);
        assert_eq!(
            scheduler_queued.payload["resource_limit_source"],
            "provider_registry"
        );
        assert_eq!(
            scheduler_queued.payload["work_item"]["provider_selector"],
            "provider-1"
        );
        assert_eq!(
            scheduler_queued.payload["role_plan_source"],
            "provider_registry"
        );
        assert_eq!(
            scheduler_queued.payload["role_plan"]["reason_code"],
            "role_allocation_resolved_with_model_reuse"
        );
        assert_eq!(
            scheduler_queued.payload["role_plan"]["distinct_model_count"],
            1
        );
        assert_eq!(
            scheduler_queued.payload["role_plan"]["reused_model_count"],
            3
        );
        assert_eq!(
            scheduler_queued.payload["role_plan"]["assignments"][0]["role"],
            "planning"
        );
        assert_eq!(
            scheduler_queued.payload["role_plan"]["assignments"][0]["selected_model_id"],
            "provider-1/code-model"
        );
        assert!(
            scheduler_queued
                .evidence_refs
                .contains(&"scheduler:provider-registry-concurrency".to_string())
        );
        assert!(
            scheduler_queued
                .evidence_refs
                .contains(&"provider:role-routing".to_string())
        );
        assert!(
            scheduler_queued
                .evidence_refs
                .contains(&"provider:single-model-role-reuse".to_string())
        );
        assert_eq!(scheduler_queued.payload["role_work_item_count"], 4);
        let role_child_events: Vec<_> = events
            .iter()
            .filter(|event| event.payload["source"] == "conversation.role_plan")
            .collect();
        assert_eq!(role_child_events.len(), 4);
        assert_eq!(role_child_events[0].payload["role"], "planning");
        assert_eq!(
            role_child_events[0].payload["parent_work_item_id"],
            scheduler_root_id
        );
        assert_eq!(
            role_child_events[0].payload["model_id"],
            "provider-1/code-model"
        );
        assert_eq!(
            role_child_events[0].payload["work_item"]["dependencies"][0],
            scheduler_root_id
        );
        assert!(
            role_child_events[0]
                .evidence_refs
                .contains(&"scheduler:role-work-item-queued".to_string())
        );
        assert!(role_child_events.iter().skip(1).all(|event| {
            event
                .evidence_refs
                .contains(&"provider:single-model-role-reuse".to_string())
        }));
        let role_scheduler_completions: Vec<_> = events
            .iter()
            .filter(|event| event.kind == EventKind::WorkItemCompleted)
            .filter(|event| {
                event
                    .payload
                    .get("work_item_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|work_item_id| work_item_id.starts_with("turn-role-"))
            })
            .collect();
        assert_eq!(role_scheduler_completions.len(), 4);
        assert!(role_scheduler_completions.iter().all(|event| {
            event
                .evidence_refs
                .contains(&"daemon:role-worker-executed".to_string())
        }));
        assert!(role_scheduler_completions.iter().all(|event| {
            event
                .evidence_refs
                .contains(&"daemon:role-worker-model-call".to_string())
        }));
        assert!(role_scheduler_completions.iter().all(|event| {
            event
                .evidence_refs
                .contains(&"daemon:role-worker-parallel-batch".to_string())
        }));
        let role_timeline_completions: Vec<_> = events
            .iter()
            .filter(|event| event.payload["agent_event_kind"] == "worker_completed")
            .filter(|event| event.payload["payload"]["code"] == "role_worker_completed")
            .collect();
        assert_eq!(role_timeline_completions.len(), 4);
        assert_eq!(
            role_timeline_completions[0].payload["payload"]["model_id"],
            "provider-1/code-model"
        );
        let root_context_ref = scheduler_queued.payload["work_item"]["context_pack_ref"]
            .as_str()
            .expect("root context ref");
        let root_context_path = workspace.join(
            root_context_ref
                .strip_prefix("artifact://")
                .expect("root context artifact ref"),
        );
        let root_context_pack: opensks_contracts::ContextPack = serde_json::from_str(
            &std::fs::read_to_string(root_context_path).expect("root context pack"),
        )
        .expect("root context json");
        let mut materialized_worker_pack_ids = std::collections::BTreeSet::new();
        for event in &role_timeline_completions {
            let payload = &event.payload["payload"];
            assert_eq!(payload["worker_context_pack_materialized"], true);
            let worker_ref = payload["worker_context_pack_ref"]
                .as_str()
                .expect("worker context ref");
            assert!(
                worker_ref
                    .starts_with("artifact://.opensks/wiki/context-packs/generated/turn-context-"),
                "{worker_ref}"
            );
            assert!(worker_ref.contains("--worker-turn-role-"), "{worker_ref}");
            assert!(
                worker_ref.contains("#work_item_id=turn-role-"),
                "{worker_ref}"
            );
            let worker_artifact_ref = worker_ref.split('#').next().expect("worker artifact ref");
            let worker_context_path = workspace.join(
                worker_artifact_ref
                    .strip_prefix("artifact://")
                    .expect("worker context artifact ref"),
            );
            let worker_pack: opensks_contracts::ContextPack = serde_json::from_str(
                &std::fs::read_to_string(worker_context_path).expect("worker context pack"),
            )
            .expect("worker context json");
            materialized_worker_pack_ids.insert(worker_pack.id.clone());
            assert!(worker_pack.estimated_tokens <= worker_pack.token_budget);
            assert!(worker_pack.token_budget <= root_context_pack.token_budget.max(1));
            assert!(worker_pack.body.contains("## Worker Scope"));
            assert!(worker_pack.body.contains(&root_context_pack.id));
            assert!(
                worker_pack.body.contains(
                    payload["work_item_id"]
                        .as_str()
                        .expect("worker completion id")
                )
            );
            assert!(worker_pack.body.contains(&format!(
                "role: {}",
                payload["role"].as_str().expect("worker role")
            )));
            assert!(
                !worker_pack.body.contains("## TriWiki:"),
                "worker packs should not embed full TriWiki prose entries"
            );
            assert!(
                worker_pack
                    .evidence_refs
                    .contains(&"opensks-context:worker-scoped-context-pack".to_string())
            );
        }
        assert_eq!(materialized_worker_pack_ids.len(), 4);
        assert!(role_timeline_completions.iter().all(|event| {
            event
                .evidence_refs
                .contains(&"daemon:role-worker-executed".to_string())
        }));
        assert!(role_timeline_completions.iter().all(|event| {
            event
                .evidence_refs
                .contains(&"daemon:role-worker-model-call".to_string())
        }));
        assert!(role_timeline_completions.iter().all(|event| {
            event.payload["payload"]["model_call"] == true
                && event.payload["payload"]["parallel_batch"] == true
                && event.payload["payload"]["parallel_batch_size"] == 2
                && event.payload["payload"]["parallel_lane_index"]
                    .as_u64()
                    .is_some_and(|lane| lane < 2)
                && event.payload["payload"]["response_hash"]
                    .as_str()
                    .is_some_and(|hash| hash.starts_with("fnv64:"))
                && event.payload["payload"]["response_bytes"]
                    .as_u64()
                    .is_some_and(|bytes| bytes > 0)
        }));
        let verifier_role_completions: Vec<_> = role_timeline_completions
            .iter()
            .filter(|event| event.payload["payload"]["role"] == "verification")
            .collect();
        assert_eq!(verifier_role_completions.len(), 1);
        let verifier_completion = verifier_role_completions[0];
        assert_eq!(
            verifier_completion.payload["payload"]["semantic_verifier_judgment_state"],
            "judgment_ready"
        );
        let semantic_judgment_ref =
            verifier_completion.payload["payload"]["semantic_verifier_judgment_ref"]
                .as_str()
                .expect("semantic verifier judgment ref");
        assert!(
            semantic_judgment_ref.starts_with("artifact://.opensks/runtime/semantic-verifiers/"),
            "{semantic_judgment_ref}"
        );
        let semantic_judgment_path = workspace.join(
            semantic_judgment_ref
                .strip_prefix("artifact://")
                .expect("semantic judgment artifact ref"),
        );
        let semantic_judgment_raw = std::fs::read_to_string(semantic_judgment_path)
            .expect("semantic verifier judgment receipt");
        assert!(!semantic_judgment_raw.contains("fixture-credential"));
        assert!(!semantic_judgment_raw.contains(workspace.to_string_lossy().as_ref()));
        assert!(!semantic_judgment_raw.contains("Role subcall"));
        let semantic_judgment: opensks_contracts::SemanticVerifierJudgmentReceipt =
            serde_json::from_str(&semantic_judgment_raw).expect("semantic verifier judgment json");
        assert_eq!(
            semantic_judgment.schema,
            opensks_contracts::SEMANTIC_VERIFIER_JUDGMENT_SCHEMA
        );
        assert_eq!(semantic_judgment.role, "verification");
        assert_eq!(semantic_judgment.verifier_kind, "model_semantic_judgment");
        assert_eq!(semantic_judgment.state, "judgment_ready");
        assert_eq!(semantic_judgment.verdict, "pass");
        assert!(
            semantic_judgment
                .passed_gates
                .contains(&"semantic_verifier_verdict_passed".to_string())
        );
        assert!(semantic_judgment.failed_gates.is_empty());
        assert_eq!(semantic_judgment.provider_id.as_deref(), Some("provider-1"));
        assert_eq!(
            semantic_judgment.model_id.as_deref(),
            Some("provider-1/code-model")
        );
        assert_eq!(
            semantic_judgment.response_hash,
            verifier_completion.payload["payload"]["response_hash"]
                .as_str()
                .expect("response hash")
        );
        assert!(semantic_judgment.response_bytes > 0);
        assert!(semantic_judgment.path_redacted);
        assert!(semantic_judgment.content_redacted);
        assert!(
            semantic_judgment
                .evidence_refs
                .contains(&"daemon:semantic-verifier-judgment".to_string())
        );
        assert!(role_scheduler_completions.iter().any(|event| {
            event
                .evidence_refs
                .contains(&"daemon:semantic-verifier-judgment".to_string())
        }));
        let code_role_completions: Vec<_> = role_timeline_completions
            .iter()
            .filter(|event| event.payload["payload"]["role"] == "code")
            .collect();
        assert_eq!(code_role_completions.len(), 1);
        let code_role_completion = code_role_completions[0];
        assert_eq!(
            code_role_completion.payload["payload"]["code_candidate_target_count"],
            1
        );
        let code_candidate_ref = code_role_completion.payload["payload"]["code_candidate_ref"]
            .as_str()
            .expect("code role candidate ref");
        let code_candidate_patch_ref =
            code_role_completion.payload["payload"]["code_candidate_patch_ref"]
                .as_str()
                .expect("code role candidate patch ref");
        let code_candidate_path = workspace.join(
            code_candidate_ref
                .strip_prefix("artifact://")
                .expect("artifact candidate ref"),
        );
        let code_candidate_patch_path = workspace.join(
            code_candidate_patch_ref
                .strip_prefix("artifact://")
                .expect("artifact patch ref"),
        );
        let code_candidate: opensks_contracts::RoleSubcontractCandidateReceipt =
            serde_json::from_str(
                &std::fs::read_to_string(code_candidate_path).expect("role candidate receipt"),
            )
            .expect("role candidate json");
        assert_eq!(
            code_candidate.schema,
            opensks_contracts::ROLE_SUBCONTRACT_CANDIDATE_RECEIPT_SCHEMA
        );
        assert_eq!(code_candidate.role, "code");
        assert!(!code_candidate.main_workspace_modified);
        assert_eq!(code_candidate.target_paths[0], "ROLE_CODE_NOTE.md");
        assert!(
            std::fs::read_to_string(code_candidate_patch_path)
                .expect("role candidate patch")
                .contains("code role candidate")
        );
        assert!(events.iter().any(|event| {
            event.payload["worker_id"]
                .as_str()
                .is_some_and(|worker_id| worker_id.starts_with("role-subcontract-"))
                && event.payload["agent_event_kind"] == "file_patch_applied"
                && event.payload["payload"]["applied_files"][0] == "ROLE_CODE_NOTE.md"
        }));
        assert!(
            !workspace.join("ROLE_CODE_NOTE.md").exists(),
            "role subcontract candidate must stay out of the main workspace"
        );
        assert!(role_scheduler_completions.iter().any(|event| {
            event
                .evidence_refs
                .contains(&"daemon:role-worker-code-candidate".to_string())
        }));
        let events_json = serde_json::to_string(&events).expect("events json");
        assert!(!events_json.contains("fixture-credential"));
        assert!(!events_json.contains(workspace.to_string_lossy().as_ref()));
        assert!(events.iter().any(|event| {
            event.payload["agent_event_kind"] == "worker_spawned"
                && event.payload["payload"]["code"] == "execution_workspace_prepared"
                && event.payload["payload"]["path_redacted"] == true
        }));
        assert!(events.iter().any(|event| {
            event.payload["agent_event_kind"] == "worker_progress"
                && event.payload["payload"]["code"] == "integration_candidate_ready"
                && event.payload["payload"]["approval_required"] == true
        }));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_executes_objective_plan_worker_and_verifier_children() {
        let workspace = git_workspace("conversation-supervisor-objective-runtime");
        seed_objective_runtime_targets(&workspace);
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let (endpoint, max_active_role_calls) = spawn_chat_completion_server_with_role_requests(
            "ROUTED_NOTE.md",
            "objective route dispatched",
            8,
        );
        seed_healthy_provider_model_with_endpoint(&workspace, &endpoint, true);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Pinned,
                    model_id: Some("provider-1/code-model".to_string()),
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Standard,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "objective-planner".to_string(),
                max_parallelism: 1,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: None,
                updated_at_ms: 1_960,
            };
            repo.set_thread_settings(
                &conversation_id,
                &serde_json::to_string(&thread_settings).expect("settings json"),
                1_960,
            )
            .expect("set objective settings");
        }

        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-objective-runtime-start",
            "idem-conversation-objective-runtime-start",
        );
        turn_request.message.text =
            "Append an objective runtime note to ROUTED_NOTE.md".to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-objective-runtime-tick",
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
        assert!(
            max_active_role_calls.load(Ordering::SeqCst) >= 1,
            "objective worker/verifier path should use the provider-backed role server"
        );
        assert!(!output.contains("fixture-credential"));
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["executed"]["status"], "executed", "{output}");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");

        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(&accepted[0].run_id)
            .expect("replay events");
        let objective_completions: Vec<_> = events
            .iter()
            .filter(|event| event.kind == EventKind::WorkItemCompleted)
            .filter(|event| {
                event
                    .payload
                    .get("work_item_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| id.starts_with("work-template-"))
            })
            .collect();
        let objective_ids = objective_completions
            .iter()
            .filter_map(|event| event.payload["work_item_id"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(objective_ids.contains("work-template-goal"));
        assert!(objective_ids.contains("work-template-workers"));
        assert!(objective_ids.contains("work-template-verifier"));
        assert!(objective_ids.contains("work-template-apply"));
        assert!(objective_ids.contains("work-template-seal"));
        assert!(objective_completions.iter().all(|event| {
            event
                .evidence_refs
                .contains(&"scheduler:objective-plan-work-item".to_string())
                && event
                    .evidence_refs
                    .contains(&"daemon:objective-plan-child-executed".to_string())
        }));
        let objective_worker_completion = objective_completions
            .iter()
            .find(|event| event.payload["work_item_id"] == "work-template-workers")
            .expect("objective worker completion");
        assert!(
            objective_worker_completion
                .evidence_refs
                .contains(&"daemon:role-worker-code-candidate".to_string())
        );
        let objective_verifier_completion = objective_completions
            .iter()
            .find(|event| event.payload["work_item_id"] == "work-template-verifier")
            .expect("objective verifier completion");
        assert!(
            objective_verifier_completion
                .evidence_refs
                .contains(&"daemon:semantic-verifier-judgment".to_string())
        );
        let objective_apply_completion = objective_completions
            .iter()
            .find(|event| event.payload["work_item_id"] == "work-template-apply")
            .expect("objective apply completion");
        assert!(
            objective_apply_completion
                .evidence_refs
                .contains(&"daemon:objective-plan-apply-runtime".to_string())
        );
        assert!(
            objective_apply_completion
                .evidence_refs
                .contains(&"daemon:objective-plan-apply-awaiting-approval".to_string())
        );
        let objective_seal_completion = objective_completions
            .iter()
            .find(|event| event.payload["work_item_id"] == "work-template-seal")
            .expect("objective seal completion");
        assert!(
            objective_seal_completion
                .evidence_refs
                .contains(&"daemon:objective-plan-seal-runtime".to_string())
        );
        assert!(
            objective_seal_completion
                .evidence_refs
                .contains(&"daemon:objective-plan-seal-pending-apply".to_string())
        );

        let objective_candidate_path = workspace
            .join(".opensks")
            .join("runtime")
            .join("role-candidates")
            .join(&accepted[0].run_id)
            .join("work-template-workers")
            .join("candidate.json");
        let objective_candidate: opensks_contracts::RoleSubcontractCandidateReceipt =
            serde_json::from_str(
                &std::fs::read_to_string(&objective_candidate_path)
                    .expect("objective worker role candidate"),
            )
            .expect("objective worker candidate json");
        assert_eq!(objective_candidate.work_item_id, "work-template-workers");
        assert_eq!(objective_candidate.role, "code");
        assert!(objective_candidate.shard_policy_id.is_some());
        assert!(!objective_candidate.main_workspace_modified);

        let objective_judgment_path = workspace
            .join(".opensks")
            .join("runtime")
            .join("semantic-verifiers")
            .join(&accepted[0].run_id)
            .join("work-template-verifier")
            .join("judgment.json");
        let objective_judgment: opensks_contracts::SemanticVerifierJudgmentReceipt =
            serde_json::from_str(
                &std::fs::read_to_string(&objective_judgment_path)
                    .expect("objective verifier judgment"),
            )
            .expect("objective verifier judgment json");
        assert_eq!(objective_judgment.role, "verification");
        assert_eq!(objective_judgment.verdict, "pass");

        let candidate_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(&accepted[0].run_id);
        let aggregate_candidate: opensks_contracts::IntegrationCandidateReceipt =
            serde_json::from_str(
                &std::fs::read_to_string(candidate_dir.join("candidate.json"))
                    .expect("aggregate candidate"),
            )
            .expect("aggregate candidate json");
        assert!(aggregate_candidate.aggregate_candidate_count >= 2);
        assert!(aggregate_candidate.source_candidates.iter().any(|source| {
            source.source == "role_subcontract"
                && source.work_item_id.as_deref() == Some("work-template-workers")
                && source.role.as_deref() == Some("code")
        }));
        assert!(aggregate_candidate.shard_policy_id.is_some());
        assert_eq!(
            aggregate_candidate.reason_code,
            "aggregate_isolated_patch_candidate_ready"
        );
        let integration: opensks_contracts::IntegrationApplyReceipt = serde_json::from_str(
            &std::fs::read_to_string(candidate_dir.join("integration.json"))
                .expect("objective pending integration receipt"),
        )
        .expect("objective pending integration json");
        assert_eq!(integration.state, "awaiting_approval");
        assert_eq!(integration.reason_code, "approval_not_persisted");
        assert!(!integration.main_workspace_modified);
        assert_eq!(
            std::fs::read_to_string(workspace.join("ROLE_CODE_NOTE.md"))
                .expect("role note should remain tracked"),
            "before role\n",
            "approval-pending objective apply must not mutate the main workspace"
        );
        let events_json = serde_json::to_string(&events).expect("events json");
        assert!(!events_json.contains("fixture-credential"));
        assert!(!events_json.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_executes_objective_plan_approved_apply_and_seal_children() {
        let workspace = git_workspace("conversation-supervisor-objective-apply-seal");
        seed_objective_runtime_targets(&workspace);
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let (endpoint, _max_active_role_calls) = spawn_chat_completion_server_with_role_requests(
            "ROUTED_NOTE.md",
            "objective approved route dispatched",
            8,
        );
        seed_healthy_provider_model_with_endpoint(&workspace, &endpoint, true);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Pinned,
                    model_id: Some("provider-1/code-model".to_string()),
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Standard,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "objective-planner".to_string(),
                max_parallelism: 1,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: None,
                updated_at_ms: 1_970,
            };
            repo.set_thread_settings(
                &conversation_id,
                &serde_json::to_string(&thread_settings).expect("settings json"),
                1_970,
            )
            .expect("set objective settings");
        }

        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-objective-apply-seal-start",
            "idem-conversation-objective-apply-seal-start",
        );
        turn_request.message.text =
            "Append an approved objective runtime note to ROUTED_NOTE.md".to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let start_output = run_stdio(
            &(serde_json::to_string(&start).expect("start request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("start stdio");
        let accepted = accepted_lines(&start_output);
        assert_eq!(accepted.len(), 1);

        let approval = integration_approval_request(&accepted[0].run_id);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-objective-apply-seal-tick",
            "daemon-test-supervisor",
            1_000,
        );
        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval request"),
                serde_json::to_string(&tick).expect("tick request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("tick stdio");
        assert!(!output.contains("fixture-credential"));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["executed"]["status"], "executed");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");

        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(&accepted[0].run_id)
            .expect("replay events");
        let objective_completions: Vec<_> = events
            .iter()
            .filter(|event| event.kind == EventKind::WorkItemCompleted)
            .filter(|event| {
                event
                    .payload
                    .get("work_item_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| id.starts_with("work-template-"))
            })
            .collect();
        let objective_apply_completion = objective_completions
            .iter()
            .find(|event| event.payload["work_item_id"] == "work-template-apply")
            .expect("objective apply completion");
        assert!(
            objective_apply_completion
                .evidence_refs
                .contains(&"daemon:objective-plan-apply-runtime".to_string())
        );
        assert!(
            objective_apply_completion
                .evidence_refs
                .contains(&"integration:approval-gated-apply".to_string())
        );
        let objective_seal_completion = objective_completions
            .iter()
            .find(|event| event.payload["work_item_id"] == "work-template-seal")
            .expect("objective seal completion");
        assert!(
            objective_seal_completion
                .evidence_refs
                .contains(&"daemon:objective-plan-seal-runtime".to_string())
        );
        assert!(
            objective_seal_completion
                .evidence_refs
                .contains(&"integration:final-seal".to_string())
        );

        let candidate_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(&accepted[0].run_id);
        let integration: opensks_contracts::IntegrationApplyReceipt = serde_json::from_str(
            &std::fs::read_to_string(candidate_dir.join("integration.json"))
                .expect("objective integrated receipt"),
        )
        .expect("objective integrated json");
        assert_eq!(integration.state, "integrated");
        assert!(integration.main_workspace_modified);
        assert!(integration.seal_ref.is_some());
        assert!(candidate_dir.join("seal.json").exists());
        assert!(
            workspace.join("ROLE_CODE_NOTE.md").exists(),
            "approved objective apply should mutate the main workspace through integration"
        );
        let seal: opensks_contracts::IntegrationFinalSeal = serde_json::from_str(
            &std::fs::read_to_string(candidate_dir.join("seal.json"))
                .expect("objective final seal"),
        )
        .expect("objective final seal json");
        assert_eq!(seal.state, "sealed");
        assert_eq!(seal.reason_code, "integration_final_sealed");
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_supervisor_tick_dispatches_provider_image_tools_end_to_end() {
        let workspace = temp_workspace("conversation-supervisor-provider-image-tools");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let (endpoint, server) = spawn_provider_image_tool_conversation_server();
        seed_healthy_code_image_vision_provider_models_with_endpoint(&workspace, &endpoint, true);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Pinned,
                    model_id: Some("provider-1/code-model".to_string()),
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Standard,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "provider-image-tools-e2e".to_string(),
                max_parallelism: 2,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: Some("provider-1/selected-image-model".to_string()),
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
            "req-conversation-provider-image-tools-start",
            "idem-conversation-provider-image-tools-start",
        );
        turn_request.message.text =
            "Generate an image artifact, inspect it, and report the receipts.".to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-provider-image-tools-tick",
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
        let provider_requests = server.join().expect("provider requests");
        assert_eq!(provider_requests.len(), 9);
        assert!(!provider_requests.join("\n").contains("sk-"));
        assert!(!output.contains("fixture-credential"));
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["executed"]["status"], "executed");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");
        assert_eq!(
            ticks[0]["executed"]["selected_model_id"],
            "provider-1/code-model"
        );

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert!(messages.iter().any(|message| message.content_redacted
            == "Image generation and inspection receipts are complete."));
        let isolated_workspace = workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(&accepted[0].run_id)
            .join("turn-supervisor");
        let ledger_path = isolated_workspace
            .join(".opensks")
            .join("assets")
            .join("candidates")
            .join("image-ledger.json");
        let ledger: opensks_contracts::ImageLedger =
            serde_json::from_str(&std::fs::read_to_string(ledger_path).expect("ledger"))
                .expect("ledger json");
        assert_eq!(ledger.assets.len(), 1);
        assert_eq!(ledger.assets[0].id, "agent-e2e-image");
        assert_eq!(ledger.assets[0].model_id, "provider-1/selected-image-model");
        assert_eq!(ledger.provenance_receipts.len(), 2);
        assert_eq!(
            ledger.provenance_receipts[0].operation,
            opensks_contracts::ImageOperation::Generate
        );
        assert_eq!(
            ledger.provenance_receipts[0].model_id,
            "provider-1/selected-image-model"
        );
        assert_eq!(
            ledger.provenance_receipts[1].operation,
            opensks_contracts::ImageOperation::Inspect
        );
        assert_eq!(
            ledger.provenance_receipts[1].model_id,
            "provider-1/vision-model"
        );

        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(&accepted[0].run_id)
            .expect("replay events");
        let events_json = serde_json::to_string(&events).expect("events json");
        assert!(!events_json.contains("fixture-credential"));
        assert!(!events_json.contains(workspace.to_string_lossy().as_ref()));
        assert!(events.iter().any(|event| {
            event.kind == EventKind::ImageArtifactCreated
                && event.payload["agent_event_kind"] == "image_artifact_created"
                && event.payload["payload"]["asset_id"] == "agent-e2e-image"
                && event.payload["payload"]["model_id"] == "provider-1/selected-image-model"
        }));
        assert!(events.iter().any(|event| {
            event.kind == EventKind::WorkItemCompleted
                && event.payload["agent_event_kind"] == "tool_call_completed"
                && event.payload["payload"]["tool"] == "image.inspect"
                && event.payload["payload"]["model_id"] == "provider-1/vision-model"
        }));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn provider_image_tool_executor_uses_registry_route_and_writes_artifact() {
        let workspace = temp_workspace("provider-image-tool-executor");
        let (endpoint, server) = spawn_image_generation_server();
        seed_healthy_image_provider_model_with_endpoint(&workspace, &endpoint, true);

        let executor = prepare_image_tool_executor(&workspace, None).expect("image executor");
        let asset = opensks_adapter::ImageToolExecutor::generate_image(
            &executor,
            &workspace,
            &opensks_adapter::ImageGenerateToolRequest {
                prompt: "render daemon image".to_string(),
                asset_id: Some("daemon-image".to_string()),
                width: 64,
                height: 64,
            },
        )
        .expect("generated image asset");
        let request = server.join().expect("image request");

        assert!(!request.contains("sk-"));
        assert_eq!(asset.id, "daemon-image");
        assert_eq!(asset.provider_id, "provider-1");
        assert_eq!(asset.model_id, "provider-1/image-model");
        assert_eq!(asset.path, ".opensks/assets/candidates/daemon-image.png");
        let bytes = std::fs::read(workspace.join(&asset.path)).expect("image bytes");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let ledger_path = workspace
            .join(".opensks")
            .join("assets")
            .join("candidates")
            .join("image-ledger.json");
        let ledger: opensks_contracts::ImageLedger =
            serde_json::from_str(&std::fs::read_to_string(ledger_path).expect("ledger"))
                .expect("ledger json");
        assert_eq!(ledger.assets.len(), 1);
        assert_eq!(ledger.assets[0].id, "daemon-image");
        assert_eq!(ledger.provenance_receipts.len(), 1);
        assert_eq!(
            ledger.provenance_receipts[0].content_hash,
            asset.content_hash
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn provider_image_tool_executor_uses_vision_route_and_records_inspection() {
        let workspace = temp_workspace("provider-image-inspect-tool-executor");
        let (endpoint, server) = spawn_vision_completion_server();
        seed_healthy_image_and_vision_provider_models_with_endpoint(&workspace, &endpoint, true);
        let provider_repo = opensks_provider::ProviderRepository::open_workspace(&workspace)
            .expect("provider repo");
        let image_registry = opensks_provider::model_registry_from_repository(&provider_repo)
            .expect("provider model registry");
        let mut runtime = opensks_image::ImageRuntime::new();
        let asset = runtime
            .generate_asset_file(
                &image_registry,
                &workspace,
                opensks_image::ImageAssetRequest {
                    id: "daemon-inspect-image",
                    width: 16,
                    height: 16,
                    anchors: Vec::new(),
                    prompt: Some("render daemon inspection fixture"),
                },
            )
            .expect("seed image asset");
        persist_image_ledger(&workspace, runtime.ledger()).expect("seed ledger");

        let executor = prepare_image_tool_executor(&workspace, None).expect("vision executor");
        assert_eq!(
            executor.available_tool_names(),
            vec!["image.generate", "image.inspect"]
        );
        let result = opensks_adapter::ImageToolExecutor::inspect_image(
            &executor,
            &workspace,
            &opensks_adapter::ImageInspectToolRequest {
                artifact_ref: asset.id.clone(),
                prompt: Some("Describe this generated image".to_string()),
            },
        )
        .expect("inspection result");
        let request = server.join().expect("vision request");

        assert!(!request.contains("sk-"));
        assert_eq!(result.text, "The image is a generated daemon fixture.");
        assert_eq!(result.receipt.asset_id, "daemon-inspect-image");
        assert_eq!(result.receipt.provider_id, "provider-1");
        assert_eq!(result.receipt.model_id, "provider-1/vision-model");
        assert_eq!(result.receipt.content_hash, asset.content_hash);
        assert!(
            result
                .receipt
                .evidence_refs
                .contains(&"opensks-image:provider-image.inspect".to_string())
        );
        let ledger_path = workspace
            .join(".opensks")
            .join("assets")
            .join("candidates")
            .join("image-ledger.json");
        let ledger: opensks_contracts::ImageLedger =
            serde_json::from_str(&std::fs::read_to_string(ledger_path).expect("ledger"))
                .expect("ledger json");
        assert_eq!(ledger.assets.len(), 1);
        assert_eq!(ledger.provenance_receipts.len(), 2);
        assert_eq!(
            ledger.provenance_receipts[1].operation,
            opensks_contracts::ImageOperation::Inspect
        );
        assert_eq!(ledger.provenance_receipts[1].asset_id, asset.id);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_applies_approved_candidate_to_main_workspace() {
        let workspace = git_workspace("integration-candidate-apply");
        let run_id = "run-integration-apply";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
        ";
        write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);
        let turn_supervisor_isolation =
            source_isolation_path(&workspace, run_id, "turn-supervisor");
        std::fs::create_dir_all(&turn_supervisor_isolation).expect("source isolation");
        std::fs::write(turn_supervisor_isolation.join("NOTE.md"), "after\n")
            .expect("source isolation file");
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-apply",
            run_id,
            format!("approval-integration-{run_id}"),
        );
        let approval = integration_approval_request(run_id);

        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval"),
                serde_json::to_string(&request).expect("request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "integrated");
        assert_eq!(receipt.reason_code, "candidate_applied_to_main_workspace");
        assert_eq!(
            receipt.approval_policy_id.as_deref(),
            Some("safe-interactive")
        );
        assert!(receipt.repair_ref.is_none());
        let expected_verification_ref = format!(
            "artifact://.opensks/runtime/integration-candidates/{run_id}/verification.json"
        );
        assert_eq!(
            receipt.verification_ref.as_deref(),
            Some(expected_verification_ref.as_str())
        );
        let expected_seal_ref =
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/seal.json");
        assert_eq!(
            receipt.seal_ref.as_deref(),
            Some(expected_seal_ref.as_str())
        );
        let expected_cleanup_ref =
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/cleanup.json");
        assert_eq!(
            receipt.cleanup_ref.as_deref(),
            Some(expected_cleanup_ref.as_str())
        );
        assert_eq!(receipt.target_paths, vec!["NOTE.md".to_string()]);
        assert!(receipt.main_workspace_modified);
        assert!(receipt.verifier_passed);
        assert!(
            receipt
                .evidence_refs
                .contains(&"integration:path-write-lease".to_string())
        );
        assert!(
            receipt
                .evidence_refs
                .contains(&"scheduler:path-scope-bound".to_string())
        );
        assert!(
            receipt
                .evidence_refs
                .contains(&"scheduler:path-scope-external-write".to_string())
        );
        assert!(
            receipt
                .evidence_refs
                .contains(&"settings:approval-policy".to_string())
        );
        let candidate: opensks_contracts::IntegrationCandidateReceipt = serde_json::from_str(
            &std::fs::read_to_string(integration_artifact_path(
                &workspace,
                run_id,
                "candidate.json",
            ))
            .expect("candidate receipt"),
        )
        .expect("candidate json");
        assert_eq!(
            candidate.approval_policy_id.as_deref(),
            Some("safe-interactive")
        );
        assert!(
            candidate
                .evidence_refs
                .contains(&"settings:approval-policy".to_string())
        );
        assert_eq!(
            candidate
                .turn_settings
                .as_ref()
                .map(|settings| settings.pipeline_id.as_str()),
            Some("integration-test-pipeline")
        );
        assert_eq!(
            candidate
                .turn_settings
                .as_ref()
                .map(|settings| settings.max_parallelism),
            Some(6)
        );
        assert!(
            candidate
                .evidence_refs
                .contains(&"settings:turn-settings-snapshot".to_string())
        );
        let selection: opensks_contracts::IntegrationCandidateSelectionReceipt =
            serde_json::from_str(
                &std::fs::read_to_string(integration_artifact_path(
                    &workspace,
                    run_id,
                    "selection.json",
                ))
                .expect("selection receipt"),
            )
            .expect("selection json");
        assert_eq!(
            selection.approval_policy_id.as_deref(),
            Some("safe-interactive")
        );
        assert_eq!(
            selection
                .turn_settings
                .as_ref()
                .map(|settings| settings.tool_policy_id.as_str()),
            Some("integration-tools")
        );
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(
            verification.schema,
            opensks_contracts::INTEGRATION_VERIFICATION_RECEIPT_SCHEMA
        );
        assert_eq!(verification.state, "passed");
        assert_eq!(verification.reason_code, "candidate_verification_passed");
        assert_eq!(verification.planned_verifier_count, 1);
        assert_eq!(verification.passed_verifier_count, 1);
        assert_eq!(verification.failed_verifier_count, 0);
        assert_eq!(verification.verifier_lanes.len(), 1);
        assert_eq!(
            verification.verifier_lanes[0].verifier_kind,
            "read_only_git_apply_check"
        );
        assert_eq!(verification.verifier_lanes[0].state, "passed");
        assert_eq!(
            verification.verification_ref,
            expected_verification_ref.as_str()
        );
        assert!(verification.repair_ref.is_none());
        assert!(
            verification
                .passed_gates
                .iter()
                .any(|gate| gate == "git_apply_check_passed")
        );
        assert_eq!(
            receipt.integration_ref,
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/integration.json")
        );
        assert_eq!(
            receipt.final_diff_ref,
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/final.diff")
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "after\n"
        );
        let integration: opensks_contracts::IntegrationApplyReceipt = serde_json::from_str(
            &std::fs::read_to_string(
                workspace
                    .join(".opensks")
                    .join("runtime")
                    .join("integration-candidates")
                    .join(run_id)
                    .join("integration.json"),
            )
            .expect("integration receipt"),
        )
        .expect("integration json");
        assert_eq!(integration.state, "integrated");
        assert_eq!(integration.final_diff_ref, receipt.final_diff_ref);
        assert_eq!(integration.seal_ref, receipt.seal_ref);
        assert_eq!(integration.cleanup_ref, receipt.cleanup_ref);
        assert_eq!(
            integration.approval_policy_id.as_deref(),
            Some("safe-interactive")
        );
        assert_eq!(
            integration
                .turn_settings
                .as_ref()
                .map(|settings| settings.image_model_id.as_deref()),
            Some(Some("provider-1/image-model"))
        );
        assert!(
            integration
                .evidence_refs
                .contains(&"settings:turn-settings-snapshot".to_string())
        );
        assert!(
            integration
                .evidence_refs
                .contains(&"integration:path-write-lease".to_string())
        );
        let final_diff =
            std::fs::read_to_string(integration_artifact_path(&workspace, run_id, "final.diff"))
                .expect("final diff");
        assert!(final_diff.contains("diff --git a/NOTE.md b/NOTE.md"));
        assert!(final_diff.contains("-before"));
        assert!(final_diff.contains("+after"));
        let seal: opensks_contracts::IntegrationFinalSeal = serde_json::from_str(
            &std::fs::read_to_string(integration_artifact_path(&workspace, run_id, "seal.json"))
                .expect("final seal"),
        )
        .expect("seal json");
        assert_eq!(
            seal.schema,
            opensks_contracts::INTEGRATION_FINAL_SEAL_SCHEMA
        );
        assert_eq!(seal.state, "sealed");
        assert_eq!(seal.reason_code, "integration_final_sealed");
        assert_eq!(seal.candidate_id, receipt.candidate_id);
        assert_eq!(seal.target_paths, vec!["NOTE.md".to_string()]);
        assert_eq!(seal.seal_ref, expected_seal_ref);
        assert_eq!(seal.verification_ref, expected_verification_ref);
        assert_eq!(seal.integration_ref, receipt.integration_ref);
        assert_eq!(seal.final_diff_ref, receipt.final_diff_ref);
        assert_eq!(seal.approval_policy_id.as_deref(), Some("safe-interactive"));
        assert_eq!(
            seal.turn_settings
                .as_ref()
                .map(|settings| settings.verifier_count),
            Some(3)
        );
        assert!(seal.repair_ref.is_none());
        assert_eq!(
            seal.cleanup_ref.as_deref(),
            Some(expected_cleanup_ref.as_str())
        );
        assert!(seal.failed_gates.is_empty());
        assert!(
            seal.passed_gates
                .iter()
                .any(|gate| gate == "approval_event_persisted")
        );
        assert!(
            seal.passed_gates
                .iter()
                .any(|gate| gate == "verification_receipt_passed")
        );
        assert!(
            seal.passed_gates
                .iter()
                .any(|gate| gate == "final_diff_captured")
        );
        assert!(
            seal.passed_gates
                .iter()
                .any(|gate| gate == "repair_not_required")
        );
        assert!(
            seal.passed_gates
                .iter()
                .any(|gate| gate == "source_isolation_cleanup_receipt")
        );
        assert!(
            seal.passed_gates
                .iter()
                .any(|gate| gate == "approval_policy_bound")
        );
        assert!(
            seal.passed_gates
                .iter()
                .any(|gate| gate == "turn_settings_snapshot_bound")
        );
        let cleanup = read_integration_cleanup(&workspace, run_id);
        assert_eq!(
            cleanup.schema,
            opensks_contracts::INTEGRATION_CLEANUP_RECEIPT_SCHEMA
        );
        assert_eq!(cleanup.state, "cleaned");
        assert_eq!(cleanup.cleanup_ref, expected_cleanup_ref);
        assert_eq!(cleanup.integration_ref, receipt.integration_ref);
        assert_eq!(cleanup.seal_ref, expected_seal_ref);
        assert_eq!(cleanup.cleanup_target_count, 1);
        assert_eq!(cleanup.cleaned_count, 1);
        assert_eq!(cleanup.retained_candidate_ref, receipt.candidate_ref);
        assert_eq!(cleanup.retained_patch_ref, receipt.patch_ref);
        assert_eq!(cleanup.retained_final_diff_ref, receipt.final_diff_ref);
        assert_eq!(cleanup.source_isolations.len(), 1);
        assert_eq!(cleanup.source_isolations[0].worker_id, "turn-supervisor");
        assert_eq!(
            cleanup.source_isolations[0].source_isolation_id,
            format!("isolation-{run_id}-turn-supervisor")
        );
        assert!(cleanup.source_isolations[0].existed);
        assert!(cleanup.source_isolations[0].removed);
        assert!(!turn_supervisor_isolation.exists());
        assert!(seal.content_redacted);
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(run_id)
            .expect("replay events");
        assert!(events.iter().any(|event| {
            event.kind == EventKind::ApprovalApproved
                && event.payload["approval_id"] == format!("approval-integration-{run_id}")
                && event.payload["scope"] == "integration_apply"
        }));
        assert!(events.iter().any(|event| {
            event.actor == "integration-coordinator"
                && event.kind == EventKind::WorkItemCompleted
                && event.payload["source"] == "integration.candidate.apply"
                && event.payload["main_workspace_modified"] == true
                && event
                    .evidence_refs
                    .contains(&"scheduler:path-scope-bound".to_string())
                && event
                    .evidence_refs
                    .contains(&"scheduler:path-scope-external-write".to_string())
                && event.payload["path_write_lease"]["lease_type"] == "path_write"
                && event.payload["path_write_lease"]["holder"] == "integration-coordinator"
                && event.payload["path_scope"]["workspace_relative_roots"][0] == "NOTE.md"
                && event.payload["path_scope"]["allow_external_write"] == true
                && event.payload["content_redacted"] == true
                && event.payload["verification_ref"] == receipt.verification_ref.as_deref().unwrap()
                && event.payload["final_diff_ref"] == receipt.final_diff_ref
                && event.payload["seal_ref"] == receipt.seal_ref.as_deref().unwrap()
                && event.payload["cleanup_ref"] == receipt.cleanup_ref.as_deref().unwrap()
        }));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_applies_approved_aggregate_candidate_to_main_workspace() {
        let workspace = git_workspace("integration-candidate-aggregate-apply");
        std::fs::write(workspace.join("EXTRA.md"), "old\n").expect("seed extra");
        run_git(&workspace, &["add", "EXTRA.md"]);
        run_git(&workspace, &["commit", "-m", "seed extra"]);
        let run_id = "run-integration-aggregate-apply";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
diff --git a/EXTRA.md b/EXTRA.md
--- a/EXTRA.md
+++ b/EXTRA.md
@@ -1 +1 @@
-old
+new
        ";
        write_aggregate_candidate_fixture(&workspace, run_id, &["NOTE.md", "EXTRA.md"], patch);
        let turn_supervisor_isolation =
            source_isolation_path(&workspace, run_id, "turn-supervisor");
        let role_isolation =
            source_isolation_path(&workspace, run_id, "role-subcontract-turn-role-code");
        std::fs::create_dir_all(&turn_supervisor_isolation).expect("turn isolation");
        std::fs::create_dir_all(&role_isolation).expect("role isolation");
        std::fs::write(turn_supervisor_isolation.join("NOTE.md"), "after\n")
            .expect("turn isolation file");
        std::fs::write(role_isolation.join("EXTRA.md"), "new\n").expect("role isolation file");
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-aggregate-apply",
            run_id,
            format!("approval-integration-{run_id}"),
        );
        let approval = integration_approval_request(run_id);

        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval"),
                serde_json::to_string(&request).expect("request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "integrated");
        assert_eq!(receipt.reason_code, "candidate_applied_to_main_workspace");
        assert_eq!(
            receipt.target_paths,
            vec!["NOTE.md".to_string(), "EXTRA.md".to_string()]
        );
        assert!(receipt.verifier_passed);
        assert!(receipt.main_workspace_modified);
        let expected_cleanup_ref =
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/cleanup.json");
        assert_eq!(
            receipt.cleanup_ref.as_deref(),
            Some(expected_cleanup_ref.as_str())
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "after\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("EXTRA.md")).expect("extra"),
            "new\n"
        );
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(verification.state, "passed");
        assert_eq!(verification.planned_verifier_count, 3);
        assert_eq!(verification.passed_verifier_count, 3);
        assert_eq!(verification.failed_verifier_count, 0);
        assert_eq!(verification.verifier_lanes.len(), 3);
        assert!(
            verification
                .passed_gates
                .contains(&"read_only_verifier_lanes_passed".to_string())
        );
        assert_eq!(
            verification.target_paths,
            vec!["NOTE.md".to_string(), "EXTRA.md".to_string()]
        );
        let seal: opensks_contracts::IntegrationFinalSeal = serde_json::from_str(
            &std::fs::read_to_string(integration_artifact_path(&workspace, run_id, "seal.json"))
                .expect("final seal"),
        )
        .expect("seal json");
        assert_eq!(seal.state, "sealed");
        assert_eq!(seal.cleanup_ref, receipt.cleanup_ref);
        assert_eq!(
            seal.target_paths,
            vec!["NOTE.md".to_string(), "EXTRA.md".to_string()]
        );
        let cleanup = read_integration_cleanup(&workspace, run_id);
        assert_eq!(cleanup.state, "cleaned");
        assert_eq!(cleanup.cleanup_target_count, 2);
        assert_eq!(cleanup.cleaned_count, 2);
        let cleanup_workers = cleanup
            .source_isolations
            .iter()
            .map(|target| target.worker_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            cleanup_workers,
            std::collections::BTreeSet::from([
                "role-subcontract-turn-role-code",
                "turn-supervisor"
            ])
        );
        assert!(!turn_supervisor_isolation.exists());
        assert!(!role_isolation.exists());
        let final_diff =
            std::fs::read_to_string(integration_artifact_path(&workspace, run_id, "final.diff"))
                .expect("final diff");
        assert!(final_diff.contains("diff --git a/NOTE.md b/NOTE.md"));
        assert!(final_diff.contains("diff --git a/EXTRA.md b/EXTRA.md"));
        assert!(final_diff.contains("+after"));
        assert!(final_diff.contains("+new"));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_accepts_passing_semantic_verifier_judgment() {
        let workspace = git_workspace("integration-candidate-semantic-pass");
        let run_id = "run-integration-semantic-pass";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
";
        write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);
        write_semantic_verifier_judgment_fixture(&workspace, run_id, "pass");
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-semantic-pass",
            run_id,
            format!("approval-integration-{run_id}"),
        );
        let approval = integration_approval_request(run_id);

        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval"),
                serde_json::to_string(&request).expect("request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "integrated");
        assert_eq!(receipt.reason_code, "candidate_applied_to_main_workspace");
        assert!(receipt.main_workspace_modified);
        assert!(receipt.verifier_passed);
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(verification.state, "passed");
        assert_eq!(verification.reason_code, "candidate_verification_passed");
        assert_eq!(verification.planned_verifier_count, 2);
        assert_eq!(verification.passed_verifier_count, 2);
        assert_eq!(verification.failed_verifier_count, 0);
        assert_eq!(verification.verifier_lanes.len(), 2);
        assert!(verification.verifier_lanes.iter().any(|lane| {
            lane.verifier_kind == "model_semantic_judgment"
                && lane.state == "passed"
                && lane
                    .passed_gates
                    .contains(&"semantic_verifier_gate_passed".to_string())
        }));
        assert!(
            verification
                .passed_gates
                .contains(&"semantic_verifier_gate_passed".to_string())
        );
        assert!(
            verification
                .evidence_refs
                .contains(&"integration:semantic-verifier-gate".to_string())
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "after\n"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_blocks_failed_semantic_verifier_judgment() {
        let workspace = git_workspace("integration-candidate-semantic-fail");
        let run_id = "run-integration-semantic-fail";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
";
        write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);
        write_semantic_verifier_judgment_fixture(&workspace, run_id, "fail");
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-semantic-fail",
            run_id,
            format!("approval-integration-{run_id}"),
        );
        let approval = integration_approval_request(run_id);

        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval"),
                serde_json::to_string(&request).expect("request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "failed");
        assert_eq!(receipt.reason_code, "semantic_verifier_rejected");
        assert!(!receipt.main_workspace_modified);
        assert!(!receipt.verifier_passed);
        assert!(receipt.seal_ref.is_none());
        assert!(receipt.cleanup_ref.is_none());
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(verification.state, "failed");
        assert_eq!(verification.reason_code, "semantic_verifier_rejected");
        assert_eq!(verification.planned_verifier_count, 2);
        assert_eq!(verification.passed_verifier_count, 1);
        assert_eq!(verification.failed_verifier_count, 1);
        assert_eq!(verification.verifier_lanes.len(), 2);
        assert!(verification.verifier_lanes.iter().any(|lane| {
            lane.verifier_kind == "model_semantic_judgment"
                && lane.state == "failed"
                && lane
                    .failed_gates
                    .contains(&"semantic_verifier_gate_passed".to_string())
        }));
        assert!(
            verification
                .failed_gates
                .contains(&"semantic_verifier_gate_passed".to_string())
        );
        assert!(
            verification
                .evidence_refs
                .contains(&"integration:semantic-verifier-gate".to_string())
        );
        assert!(
            !integration_artifact_path(&workspace, run_id, "final.diff").exists(),
            "semantic verifier failure must not write final diff evidence"
        );
        assert!(
            !integration_artifact_path(&workspace, run_id, "seal.json").exists(),
            "semantic verifier failure must not write final seal evidence"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "before\n"
        );
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_blocks_missing_planner_required_shards() {
        let workspace = git_workspace("integration-candidate-planner-shards-missing");
        let run_id = "run-integration-planner-shards-missing";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
";
        write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);
        add_planner_policy_to_candidate_fixture(&workspace, run_id, 2, 1, 3);
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-planner-shards-missing",
            run_id,
            format!("approval-integration-{run_id}"),
        );
        let approval = integration_approval_request(run_id);

        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval"),
                serde_json::to_string(&request).expect("request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "failed");
        assert_eq!(receipt.reason_code, "planner_required_shards_missing");
        assert!(!receipt.main_workspace_modified);
        assert!(!receipt.verifier_passed);
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(verification.state, "failed");
        assert_eq!(verification.reason_code, "planner_required_shards_missing");
        assert!(
            verification
                .failed_gates
                .contains(&"planner_required_shards_present".to_string())
        );
        assert!(
            verification
                .evidence_refs
                .contains(&"planner:shard-policy".to_string())
        );
        assert!(!integration_artifact_path(&workspace, run_id, "seal.json").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "before\n"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_blocks_planner_verifier_count_above_runtime_cap() {
        let workspace = git_workspace("integration-candidate-planner-verifier-cap");
        let run_id = "run-integration-planner-verifier-cap";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
";
        write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);
        add_planner_policy_to_candidate_fixture(
            &workspace,
            run_id,
            1,
            1,
            MAX_INTEGRATION_VERIFIER_LANES + 1,
        );
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-planner-verifier-cap",
            run_id,
            format!("approval-integration-{run_id}"),
        );
        let approval = integration_approval_request(run_id);

        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval"),
                serde_json::to_string(&request).expect("request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "failed");
        assert_eq!(
            receipt.reason_code,
            "planner_required_verifier_count_exceeds_runtime_cap"
        );
        assert!(!receipt.main_workspace_modified);
        assert!(!receipt.verifier_passed);
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(
            verification.reason_code,
            "planner_required_verifier_count_exceeds_runtime_cap"
        );
        assert!(
            verification
                .failed_gates
                .contains(&"planner_required_verifier_count_within_runtime_cap".to_string())
        );
        assert!(
            verification
                .evidence_refs
                .contains(&"planner:shard-policy".to_string())
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "before\n"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_refuses_dirty_main_workspace_target() {
        let workspace = git_workspace("integration-candidate-dirty-target");
        let run_id = "run-integration-dirty";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
";
        write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);
        std::fs::write(workspace.join("NOTE.md"), "user dirty\n").expect("dirty note");
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-dirty",
            run_id,
            format!("approval-integration-{run_id}"),
        );
        let approval = integration_approval_request(run_id);

        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval"),
                serde_json::to_string(&request).expect("request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "failed");
        assert_eq!(receipt.reason_code, "target_dirty");
        let expected_verification_ref = format!(
            "artifact://.opensks/runtime/integration-candidates/{run_id}/verification.json"
        );
        assert_eq!(
            receipt.verification_ref.as_deref(),
            Some(expected_verification_ref.as_str())
        );
        let expected_repair_ref =
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/repair.json");
        assert_eq!(
            receipt.repair_ref.as_deref(),
            Some(expected_repair_ref.as_str())
        );
        assert!(receipt.seal_ref.is_none());
        assert!(!receipt.main_workspace_modified);
        assert!(!receipt.verifier_passed);
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(verification.state, "failed");
        assert_eq!(verification.reason_code, "target_dirty");
        assert_eq!(verification.planned_verifier_count, 1);
        assert_eq!(verification.passed_verifier_count, 0);
        assert_eq!(verification.failed_verifier_count, 1);
        assert_eq!(verification.verifier_lanes.len(), 1);
        assert_eq!(verification.verifier_lanes[0].state, "failed");
        assert_eq!(
            verification.repair_ref.as_deref(),
            Some(expected_repair_ref.as_str())
        );
        assert!(
            verification
                .failed_gates
                .iter()
                .any(|gate| gate == "git_apply_check_passed")
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "user dirty\n"
        );
        let integration: opensks_contracts::IntegrationApplyReceipt = serde_json::from_str(
            &std::fs::read_to_string(
                workspace
                    .join(".opensks")
                    .join("runtime")
                    .join("integration-candidates")
                    .join(run_id)
                    .join("integration.json"),
            )
            .expect("integration receipt"),
        )
        .expect("integration json");
        assert_eq!(integration.reason_code, "target_dirty");
        assert_eq!(integration.repair_ref, receipt.repair_ref);
        assert!(integration.seal_ref.is_none());
        assert!(
            !integration_artifact_path(&workspace, run_id, "seal.json").exists(),
            "failed apply must not write final seal evidence"
        );
        let repair: opensks_contracts::IntegrationRepairItem = serde_json::from_str(
            &std::fs::read_to_string(
                workspace
                    .join(".opensks")
                    .join("runtime")
                    .join("integration-candidates")
                    .join(run_id)
                    .join("repair.json"),
            )
            .expect("repair item"),
        )
        .expect("repair json");
        assert_eq!(
            repair.schema,
            opensks_contracts::INTEGRATION_REPAIR_ITEM_SCHEMA
        );
        assert_eq!(repair.state, "repair_required");
        assert_eq!(repair.reason_code, "target_dirty");
        assert_eq!(repair.conflict_paths, vec!["NOTE.md".to_string()]);
        assert!(repair.content_redacted);
        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(run_id)
            .expect("replay events");
        assert!(events.iter().any(|event| {
            event.actor == "integration-coordinator"
                && event.kind == EventKind::VerificationFailed
                && event.payload["reason_code"] == "target_dirty"
                && event.payload["main_workspace_modified"] == false
                && event.payload["verification_ref"] == expected_verification_ref
                && event.payload["repair_ref"] == receipt.repair_ref.as_deref().unwrap()
        }));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_refuses_committed_target_drift_before_apply() {
        let workspace = git_workspace("integration-candidate-patch-conflict");
        let run_id = "run-integration-patch-conflict";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
";
        write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);
        std::fs::write(workspace.join("NOTE.md"), "changed upstream\n").expect("change note");
        run_git(&workspace, &["add", "NOTE.md"]);
        run_git(&workspace, &["commit", "-m", "upstream drift"]);
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-patch-conflict",
            run_id,
            format!("approval-integration-{run_id}"),
        );
        let approval = integration_approval_request(run_id);

        let output = run_stdio(
            &([
                serde_json::to_string(&approval).expect("approval"),
                serde_json::to_string(&request).expect("request"),
            ]
            .join("\n")
                + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "failed");
        assert_eq!(receipt.reason_code, "target_changed_since_candidate_base");
        let expected_verification_ref = format!(
            "artifact://.opensks/runtime/integration-candidates/{run_id}/verification.json"
        );
        assert_eq!(
            receipt.verification_ref.as_deref(),
            Some(expected_verification_ref.as_str())
        );
        let expected_repair_ref =
            format!("artifact://.opensks/runtime/integration-candidates/{run_id}/repair.json");
        assert_eq!(
            receipt.repair_ref.as_deref(),
            Some(expected_repair_ref.as_str())
        );
        assert!(receipt.seal_ref.is_none());
        assert!(!receipt.main_workspace_modified);
        assert!(!receipt.verifier_passed);
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(verification.state, "failed");
        assert_eq!(
            verification.reason_code,
            "target_changed_since_candidate_base"
        );
        assert_eq!(verification.planned_verifier_count, 1);
        assert_eq!(verification.passed_verifier_count, 0);
        assert_eq!(verification.failed_verifier_count, 0);
        assert!(verification.verifier_lanes.is_empty());
        assert_eq!(
            verification.repair_ref.as_deref(),
            Some(expected_repair_ref.as_str())
        );
        assert!(
            verification
                .failed_gates
                .iter()
                .any(|gate| gate == "target_base_fence")
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "changed upstream\n"
        );
        assert!(
            !integration_artifact_path(&workspace, run_id, "final.diff").exists(),
            "failed apply must not write final diff evidence"
        );
        assert!(
            !integration_artifact_path(&workspace, run_id, "seal.json").exists(),
            "failed apply must not write final seal evidence"
        );
        let repair: opensks_contracts::IntegrationRepairItem = serde_json::from_str(
            &std::fs::read_to_string(
                workspace
                    .join(".opensks")
                    .join("runtime")
                    .join("integration-candidates")
                    .join(run_id)
                    .join("repair.json"),
            )
            .expect("repair item"),
        )
        .expect("repair json");
        assert_eq!(repair.state, "repair_required");
        assert_eq!(repair.reason_code, "target_changed_since_candidate_base");
        assert_eq!(repair.target_paths, vec!["NOTE.md".to_string()]);
        assert_eq!(repair.conflict_paths, vec!["NOTE.md".to_string()]);
        assert_eq!(repair.repair_ref, receipt.repair_ref.clone().unwrap());
        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(run_id)
            .expect("replay events");
        assert!(events.iter().any(|event| {
            event.actor == "integration-coordinator"
                && event.kind == EventKind::VerificationFailed
                && event.payload["reason_code"] == "target_changed_since_candidate_base"
                && event.payload["verification_ref"] == expected_verification_ref
                && event.payload["repair_ref"] == receipt.repair_ref.as_deref().unwrap()
        }));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn integration_candidate_apply_requires_persisted_approval_event() {
        let workspace = git_workspace("integration-candidate-awaiting-approval");
        let run_id = "run-integration-awaiting-approval";
        let patch = "\
diff --git a/NOTE.md b/NOTE.md
--- a/NOTE.md
+++ b/NOTE.md
@@ -1 +1 @@
-before
+after
";
        write_candidate_fixture(&workspace, run_id, "NOTE.md", patch);
        let request = EngineRequest::integration_candidate_apply(
            "req-integration-awaiting-approval",
            run_id,
            format!("approval-integration-{run_id}"),
        );

        let output = run_stdio(
            &(serde_json::to_string(&request).expect("request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        let receipts = integration_apply_receipt_lines(&output);
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.state, "awaiting_approval");
        assert_eq!(receipt.reason_code, "approval_not_persisted");
        assert_eq!(
            receipt.approval_policy_id.as_deref(),
            Some("safe-interactive")
        );
        assert_eq!(
            receipt
                .turn_settings
                .as_ref()
                .map(|settings| settings.pipeline_id.as_str()),
            Some("integration-test-pipeline")
        );
        let expected_verification_ref = format!(
            "artifact://.opensks/runtime/integration-candidates/{run_id}/verification.json"
        );
        assert_eq!(
            receipt.verification_ref.as_deref(),
            Some(expected_verification_ref.as_str())
        );
        assert!(receipt.seal_ref.is_none());
        assert!(!receipt.main_workspace_modified);
        assert!(receipt.verifier_passed);
        assert!(
            receipt
                .evidence_refs
                .contains(&"settings:approval-policy".to_string())
        );
        assert!(
            receipt
                .evidence_refs
                .contains(&"settings:turn-settings-snapshot".to_string())
        );
        let verification = read_integration_verification(&workspace, run_id);
        assert_eq!(verification.state, "passed");
        assert_eq!(verification.reason_code, "candidate_verification_passed");
        assert_eq!(verification.planned_verifier_count, 1);
        assert_eq!(verification.passed_verifier_count, 1);
        assert_eq!(verification.failed_verifier_count, 0);
        assert!(
            !integration_artifact_path(&workspace, run_id, "seal.json").exists(),
            "awaiting approval must not write final seal evidence"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOTE.md")).expect("note"),
            "before\n"
        );
        let events = opensks_event_store::EventStore::open_workspace(&workspace)
            .expect("event store")
            .replay(run_id)
            .expect("replay events");
        assert!(events.iter().any(|event| {
            event.actor == "integration-coordinator"
                && event.kind == EventKind::ApprovalRequested
                && event.payload["reason_code"] == "approval_not_persisted"
                && event.payload["main_workspace_modified"] == false
                && event.payload["verifier_passed"] == true
                && event.payload["verification_ref"] == expected_verification_ref
        }));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn streaming_conversation_turn_start_wakes_resident_supervisor_without_explicit_tick() {
        let workspace = temp_workspace("streaming-resident-supervisor-wakeup");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-resident-supervisor-start",
            "idem-resident-supervisor-start",
        );
        turn_request.message.text = r#"{"local_test":{"op":"create_file","path":"RESIDENT_SUPERVISOR_NOTE.md","value":"written by resident daemon supervisor"}}"#.to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let input = serde_json::to_string(&start).expect("start request") + "\n";

        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(input),
            &mut output,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");

        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].request_id, "req-resident-supervisor-start");
        let accepted_pos = output
            .find(CONVERSATION_TURN_ACCEPTED_SCHEMA)
            .expect("accepted line");
        let completed_pos = output
            .find("\"event_type\":\"request_completed\"")
            .expect("start terminal marker");
        let tick_pos = output
            .find("opensks.turn-supervisor-tick.v1")
            .expect("resident tick line");
        assert!(
            accepted_pos < tick_pos,
            "accepted handle must be emitted before resident execution:\n{output}"
        );
        assert!(
            completed_pos < tick_pos,
            "start request terminal marker must precede resident execution:\n{output}"
        );
        assert!(output.contains("\"request_id\":\"req-resident-supervisor-start\""));

        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        let claimed_ticks = ticks
            .iter()
            .filter(|tick| tick["claimed"]["run_id"] == accepted[0].run_id)
            .collect::<Vec<_>>();
        assert_eq!(claimed_ticks.len(), 1);
        let tick = claimed_ticks[0];
        assert_eq!(tick["supervisor_id"], RESIDENT_TURN_SUPERVISOR_ID);
        assert_eq!(tick["executed"]["status"], "executed");
        assert_eq!(tick["executed"]["run_state"], "completed");
        assert_eq!(tick["executed"]["execution_isolated"], true);
        assert_eq!(tick["executed"]["integration_state"], "candidate_ready");

        assert!(
            !workspace.join("RESIDENT_SUPERVISOR_NOTE.md").exists(),
            "resident supervisor must not write into the user workspace"
        );
        let isolated_workspace = workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(&accepted[0].run_id)
            .join("turn-supervisor");
        let written =
            std::fs::read_to_string(isolated_workspace.join("RESIDENT_SUPERVISOR_NOTE.md"))
                .expect("isolated written file");
        assert_eq!(written, "written by resident daemon supervisor");

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let messages = repo
            .message_page(&conversation_id, None, 10)
            .expect("messages");
        assert_eq!(messages[1].state, MessageState::Complete);
        assert_eq!(
            repo.run_projection_state(&accepted[0].run_id)
                .expect("run state")
                .as_deref(),
            Some("completed")
        );
        let candidate_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("integration-candidates")
            .join(&accepted[0].run_id);
        assert!(candidate_dir.join("candidate.json").exists());
        assert!(candidate_dir.join("selection.json").exists());
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn streaming_conversation_turn_start_with_explicit_tick_does_not_strand_resident_wakeup() {
        let workspace = temp_workspace("streaming-explicit-tick-suppresses-resident");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-stream-explicit-supervisor-start",
            "idem-stream-explicit-supervisor-start",
        );
        turn_request.message.text = r#"{"local_test":{"op":"create_file","path":"EXPLICIT_STREAM_SUPERVISOR_NOTE.md","value":"written by explicit stream supervisor"}}"#.to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-stream-explicit-supervisor-tick",
            "daemon-test-stream-explicit-supervisor",
            1_000,
        );
        let input = [
            serde_json::to_string(&start).expect("start request"),
            serde_json::to_string(&tick).expect("tick request"),
        ]
        .join("\n")
            + "\n";

        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(input),
            &mut output,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");

        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);
        let ticks = supervisor_tick_lines(&output);
        let claimed_ticks = ticks
            .iter()
            .filter(|tick| {
                tick["claimed"]["run_id"] == accepted[0].run_id
                    && tick["executed"]["status"] == "executed"
                    && tick["executed"]["run_state"] == "completed"
            })
            .collect::<Vec<_>>();
        assert_eq!(
            claimed_ticks.len(),
            1,
            "explicit/resident race must execute the accepted turn exactly once: {ticks:#?}"
        );
        let executed_tick = claimed_ticks[0];
        assert!(
            executed_tick["supervisor_id"] == "daemon-test-stream-explicit-supervisor"
                || executed_tick["supervisor_id"] == RESIDENT_TURN_SUPERVISOR_ID,
            "unexpected supervisor execution owner: {executed_tick:#?}"
        );
        assert!(
            !workspace
                .join("EXPLICIT_STREAM_SUPERVISOR_NOTE.md")
                .exists(),
            "explicit stream supervisor must still preserve user workspace isolation"
        );
        let isolated_workspace = workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(&accepted[0].run_id)
            .join("turn-supervisor");
        assert_eq!(
            std::fs::read_to_string(isolated_workspace.join("EXPLICIT_STREAM_SUPERVISOR_NOTE.md"))
                .expect("isolated explicit stream write"),
            "written by explicit stream supervisor"
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn streaming_conversation_turn_start_drains_multiple_accepted_turns_without_explicit_tick() {
        let workspace = temp_workspace("streaming-turn-start-resident-drain");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let fixtures = [
            (
                "req-stream-resident-drain-1",
                "idem-stream-resident-drain-1",
                "STREAM_RESIDENT_DRAIN_ONE.md",
                "stream drain one",
            ),
            (
                "req-stream-resident-drain-2",
                "idem-stream-resident-drain-2",
                "STREAM_RESIDENT_DRAIN_TWO.md",
                "stream drain two",
            ),
        ];
        let input = fixtures
            .iter()
            .map(|(request_id, idempotency_key, path, value)| {
                let mut turn_request =
                    turn_start_request(&project_id, &conversation_id, request_id, idempotency_key);
                turn_request.message.text = format!(
                    r#"{{"local_test":{{"op":"create_file","path":"{path}","value":"{value}"}}}}"#
                );
                serde_json::to_string(&EngineRequest::conversation_turn_start(turn_request))
                    .expect("start request")
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";

        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(input),
            &mut output,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 2);
        let first_tick_pos = output
            .find("opensks.turn-supervisor-tick.v1")
            .expect("resident tick line");
        for accepted in &accepted {
            let accepted_pos = output
                .find(&format!("\"run_id\":\"{}\"", accepted.run_id))
                .expect("accepted run id");
            assert!(
                accepted_pos < first_tick_pos,
                "accepted handles must be emitted before resident drain execution:\n{output}"
            );
        }

        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 2);
        assert_eq!(
            ticks
                .iter()
                .filter(|tick| tick["supervisor_id"] == RESIDENT_TURN_SUPERVISOR_ID)
                .count(),
            2
        );
        for accepted in &accepted {
            let (_, _, path, value) = fixtures
                .iter()
                .find(|(request_id, _, _, _)| *request_id == accepted.request_id)
                .expect("fixture for accepted request");
            let claimed_ticks = ticks
                .iter()
                .filter(|tick| tick["claimed"]["run_id"] == accepted.run_id)
                .collect::<Vec<_>>();
            assert_eq!(claimed_ticks.len(), 1);
            assert_eq!(
                claimed_ticks[0]["supervisor_id"],
                RESIDENT_TURN_SUPERVISOR_ID
            );
            assert_eq!(claimed_ticks[0]["executed"]["status"], "executed");
            assert_eq!(claimed_ticks[0]["executed"]["run_state"], "completed");
            assert!(
                !workspace.join(*path).exists(),
                "resident drain must not write {path} into the user workspace"
            );
            let isolated_workspace = workspace
                .join(".opensks")
                .join("runtime")
                .join("worktrees")
                .join(&accepted.run_id)
                .join("turn-supervisor");
            assert_eq!(
                std::fs::read_to_string(isolated_workspace.join(*path))
                    .expect("isolated stream resident drain write"),
                *value
            );
        }
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn streaming_conversation_turn_start_coalesced_drain_reruns_past_single_pass_cap() {
        let workspace = temp_workspace("streaming-turn-start-resident-rerun");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let input = (0..=RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS)
            .map(|index| {
                let request_id = format!("req-stream-rerun-{index}");
                let idempotency_key = format!("idem-stream-rerun-{index}");
                let mut turn_request = turn_start_request(
                    &project_id,
                    &conversation_id,
                    &request_id,
                    &idempotency_key,
                );
                turn_request.message.text = format!("coalesced resident rerun {index}");
                serde_json::to_string(&EngineRequest::conversation_turn_start(turn_request))
                    .expect("start request")
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";

        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(input),
            &mut output,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS + 1);

        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS + 1);
        assert!(
            ticks.iter().any(|tick| {
                tick["request_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("resident-supervisor-coalesced-drain-"))
            }),
            "coalesced resident drain should emit a follow-up burst request: {ticks:#?}"
        );

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        for accepted in &accepted {
            assert_eq!(
                repo.run_projection_state(&accepted.run_id)
                    .expect("run state")
                    .as_deref(),
                Some("failed")
            );
            assert_eq!(
                ticks
                    .iter()
                    .filter(|tick| tick["claimed"]["run_id"] == accepted.run_id)
                    .count(),
                1,
                "run should be claimed exactly once: {}",
                accepted.run_id
            );
            assert_eq!(
                ticks
                    .iter()
                    .find(|tick| tick["claimed"]["run_id"] == accepted.run_id)
                    .expect("claimed tick")["executed"]["status"],
                "executed"
            );
            assert_eq!(
                ticks
                    .iter()
                    .find(|tick| tick["claimed"]["run_id"] == accepted.run_id)
                    .expect("claimed tick")["executed"]["run_state"],
                "failed"
            );
        }
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn streaming_daemon_startup_resident_drain_stops_at_bounded_cap() {
        let workspace = temp_workspace("streaming-startup-resident-drain-cap");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut queued = Vec::new();
        for index in 0..=RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS {
            let request_id = format!("req-startup-resident-cap-{index}");
            let idempotency_key = format!("idem-startup-resident-cap-{index}");
            let path = format!("STARTUP_RESIDENT_CAP_{index}.md");
            let value = format!("startup cap {index}");
            let mut turn_request =
                turn_start_request(&project_id, &conversation_id, &request_id, &idempotency_key);
            turn_request.message.text = format!(
                r#"{{"local_test":{{"op":"create_file","path":"{path}","value":"{value}"}}}}"#
            );
            let start = EngineRequest::conversation_turn_start(turn_request);
            let start_output = run_stdio(
                &(serde_json::to_string(&start).expect("start request") + "\n"),
                &DaemonOptions {
                    workspace: workspace.clone(),
                },
            )
            .expect("start stdio");
            let accepted = accepted_lines(&start_output);
            assert_eq!(accepted.len(), 1);
            queued.push((accepted[0].clone(), path, value));
        }

        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(Vec::<u8>::new()),
            &mut output,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");

        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS);
        assert_eq!(ticks[0]["request_id"], "resident-supervisor-startup");
        for (index, tick) in ticks
            .iter()
            .enumerate()
            .take(RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS)
            .skip(1)
        {
            assert_eq!(
                tick["request_id"],
                format!("resident-supervisor-startup-drain-{}", index + 1)
            );
        }

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        for (index, (accepted, path, value)) in queued.iter().enumerate() {
            if index < RESIDENT_SUPERVISOR_MAX_DRAIN_TICKS {
                assert!(
                    ticks
                        .iter()
                        .any(|tick| tick["claimed"]["run_id"] == accepted.run_id
                            && tick["executed"]["run_state"] == "completed"),
                    "run should be drained before the cap: {}",
                    accepted.run_id
                );
                assert_eq!(
                    repo.run_projection_state(&accepted.run_id)
                        .expect("completed state")
                        .as_deref(),
                    Some("completed")
                );
                let isolated_workspace = workspace
                    .join(".opensks")
                    .join("runtime")
                    .join("worktrees")
                    .join(&accepted.run_id)
                    .join("turn-supervisor");
                assert_eq!(
                    std::fs::read_to_string(isolated_workspace.join(path))
                        .expect("isolated capped resident drain write"),
                    *value
                );
            } else {
                assert_eq!(
                    repo.run_projection_state(&accepted.run_id)
                        .expect("overflow state")
                        .as_deref(),
                    Some("queued")
                );
                assert!(
                    !workspace
                        .join(".opensks")
                        .join("runtime")
                        .join("worktrees")
                        .join(&accepted.run_id)
                        .join("turn-supervisor")
                        .exists(),
                    "overflow run must not be executed after the drain cap"
                );
            }
            assert!(
                !workspace.join(path).exists(),
                "resident drain must not write {path} into the user workspace"
            );
        }
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn resident_supervisor_coalesced_drain_marks_requested_when_already_active() {
        let drain_state = ResidentSupervisorDrainState::default();
        drain_state.active.store(true, Ordering::SeqCst);
        let request = EngineRequest::conversation_supervisor_tick(
            "resident-test-already-active",
            RESIDENT_TURN_SUPERVISOR_ID,
            RESIDENT_SUPERVISOR_LEASE_TTL_MS,
        );

        let lines = resident_supervisor_coalesced_drain_lines(
            0,
            request,
            &DaemonOptions {
                workspace: PathBuf::from("."),
            },
            &drain_state,
        );

        assert!(lines.is_empty());
        assert!(drain_state.requested.load(Ordering::SeqCst));
        drain_state.active.store(false, Ordering::SeqCst);
    }

    #[test]
    fn resident_supervisor_coalesced_followup_request_ids_include_burst_id() {
        let first = resident_supervisor_coalesced_followup_request(42, 2);
        let second = resident_supervisor_coalesced_followup_request(43, 2);

        assert_eq!(first.id, "resident-supervisor-coalesced-drain-42-2");
        assert_eq!(second.id, "resident-supervisor-coalesced-drain-43-2");
        assert_ne!(first.id, second.id);
        assert_eq!(
            first.params.supervisor_id.as_deref(),
            Some(RESIDENT_TURN_SUPERVISOR_ID)
        );
    }

    #[test]
    fn streaming_daemon_startup_wakes_existing_queued_turn_without_new_turn_start() {
        let workspace = temp_workspace("streaming-startup-resident-supervisor");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-startup-resident-start",
            "idem-startup-resident-start",
        );
        turn_request.message.text = r#"{"local_test":{"op":"create_file","path":"STARTUP_RESIDENT_NOTE.md","value":"written after daemon restart"}}"#.to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let start_output = run_stdio(
            &(serde_json::to_string(&start).expect("start request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("start stdio");
        let accepted = accepted_lines(&start_output);
        assert_eq!(accepted.len(), 1);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            assert_eq!(
                repo.run_projection_state(&accepted[0].run_id)
                    .expect("queued state")
                    .as_deref(),
                Some("queued")
            );
        }

        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(Vec::<u8>::new()),
            &mut output,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");

        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0]["supervisor_id"], RESIDENT_TURN_SUPERVISOR_ID);
        assert_eq!(ticks[0]["request_id"], "resident-supervisor-startup");
        assert_eq!(ticks[0]["claimed"]["run_id"], accepted[0].run_id);
        assert_eq!(ticks[0]["executed"]["status"], "executed");
        assert_eq!(ticks[0]["executed"]["run_state"], "completed");
        assert!(
            !workspace.join("STARTUP_RESIDENT_NOTE.md").exists(),
            "startup resident supervisor must preserve user workspace isolation"
        );
        let isolated_workspace = workspace
            .join(".opensks")
            .join("runtime")
            .join("worktrees")
            .join(&accepted[0].run_id)
            .join("turn-supervisor");
        assert_eq!(
            std::fs::read_to_string(isolated_workspace.join("STARTUP_RESIDENT_NOTE.md"))
                .expect("isolated startup resident write"),
            "written after daemon restart"
        );

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        assert_eq!(
            repo.run_projection_state(&accepted[0].run_id)
                .expect("completed state")
                .as_deref(),
            Some("completed")
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn streaming_daemon_startup_drains_multiple_existing_queued_turns_until_idle() {
        let workspace = temp_workspace("streaming-startup-resident-drain");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let fixtures = [
            (
                "req-startup-resident-drain-1",
                "idem-startup-resident-drain-1",
                "STARTUP_RESIDENT_DRAIN_ONE.md",
                "startup drain one",
            ),
            (
                "req-startup-resident-drain-2",
                "idem-startup-resident-drain-2",
                "STARTUP_RESIDENT_DRAIN_TWO.md",
                "startup drain two",
            ),
        ];
        let mut accepted = Vec::new();
        for (request_id, idempotency_key, path, value) in fixtures {
            let mut turn_request =
                turn_start_request(&project_id, &conversation_id, request_id, idempotency_key);
            turn_request.message.text = format!(
                r#"{{"local_test":{{"op":"create_file","path":"{path}","value":"{value}"}}}}"#
            );
            let start = EngineRequest::conversation_turn_start(turn_request);
            let start_output = run_stdio(
                &(serde_json::to_string(&start).expect("start request") + "\n"),
                &DaemonOptions {
                    workspace: workspace.clone(),
                },
            )
            .expect("start stdio");
            let start_accepted = accepted_lines(&start_output);
            assert_eq!(start_accepted.len(), 1);
            {
                let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
                assert_eq!(
                    repo.run_projection_state(&start_accepted[0].run_id)
                        .expect("queued state")
                        .as_deref(),
                    Some("queued")
                );
            }
            accepted.push((
                start_accepted[0].clone(),
                path.to_string(),
                value.to_string(),
            ));
        }

        let mut output = Vec::new();
        run_stdio_stream(
            Cursor::new(Vec::<u8>::new()),
            &mut output,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stream stdio");
        let output = String::from_utf8(output).expect("utf8");

        let ticks = supervisor_tick_lines(&output);
        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[0]["request_id"], "resident-supervisor-startup");
        assert_eq!(
            ticks[1]["request_id"],
            "resident-supervisor-startup-drain-2"
        );
        for (accepted, path, value) in accepted {
            let tick = ticks
                .iter()
                .find(|tick| tick["claimed"]["run_id"] == accepted.run_id)
                .expect("drained tick for queued run");
            assert_eq!(tick["supervisor_id"], RESIDENT_TURN_SUPERVISOR_ID);
            assert_eq!(tick["executed"]["status"], "executed");
            assert_eq!(tick["executed"]["run_state"], "completed");
            assert!(
                !workspace.join(&path).exists(),
                "resident drain must not write {path} into the user workspace"
            );
            let isolated_workspace = workspace
                .join(".opensks")
                .join("runtime")
                .join("worktrees")
                .join(&accepted.run_id)
                .join("turn-supervisor");
            assert_eq!(
                std::fs::read_to_string(isolated_workspace.join(path))
                    .expect("isolated resident drain write"),
                value
            );
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            assert_eq!(
                repo.run_projection_state(&accepted.run_id)
                    .expect("completed state")
                    .as_deref(),
                Some("completed")
            );
        }
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
    fn conversation_supervisor_tick_blocks_local_write_when_thread_is_read_only() {
        let workspace = temp_workspace("conversation-supervisor-read-only");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Auto,
                    model_id: None,
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Standard,
                execution_mode: ExecutionMode::ReadOnly,
                pipeline_id: "read-only-test".to_string(),
                max_parallelism: 1,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
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
            "req-conversation-supervisor-read-only-start",
            "idem-conversation-supervisor-read-only-start",
        );
        turn_request.message.text =
            r#"{"local_test":{"op":"create_file","path":"READ_ONLY_BLOCKED.md","value":"must not write"}}"#
                .to_string();
        let start = EngineRequest::conversation_turn_start(turn_request);
        let tick = EngineRequest::conversation_supervisor_tick(
            "req-conversation-supervisor-read-only-tick",
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
        assert_eq!(ticks[0]["executed"]["run_state"], "failed");
        assert_eq!(ticks[0]["executed"]["execution_mode"], "read_only");
        assert!(!workspace.join("READ_ONLY_BLOCKED.md").exists());

        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted[0].run_id).expect("replay events");
        assert!(
            events.iter().any(|event| {
                event.kind == EventKind::VerificationFailed
                    && event.payload["agent_event_kind"] == "error"
                    && event.payload["payload"]["code"] == "read_only_execution_mode"
            }),
            "read-only execution policy failure must be durable: {events:#?}"
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
                token_budget: Some(150_000),
                cost_budget_usd: Some(3.5),
                timeout_ms: Some(750_000),
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
        turn_request.thread_settings_updated_at_ms = Some(1_900);
        let legacy_settings = turn_request
            .settings
            .as_mut()
            .expect("legacy settings echo");
        legacy_settings.pipeline_id = "client-request-ignored".to_string();
        legacy_settings.max_parallelism = 99;
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
        let settings_digest = repo
            .turn_settings_digest(&accepted[0].turn_id)
            .expect("turn settings digest")
            .expect("turn settings digest");
        assert_eq!(accepted[0].settings_digest, settings_digest);
        let settings: ConversationTurnSettings =
            serde_json::from_str(&settings_raw).expect("effective settings");
        assert_eq!(settings.pipeline_id, "daemon-thread-pipeline");
        assert_eq!(settings.max_parallelism, 9);
        assert_eq!(settings.verifier_count, 4);
        assert_eq!(settings.token_budget, Some(150_000));
        assert_eq!(settings.cost_budget_usd, Some(3.5));
        assert_eq!(settings.timeout_ms, Some(750_000));
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
        assert_eq!(routing.status, opensks_contracts::RoutingStatus::Requested);
        assert_eq!(
            routing.selected_model_id.as_deref(),
            Some("openrouter/daemon-code-model")
        );
        assert_eq!(
            routing.reason_code,
            "explicit_thread_settings_model_requested"
        );
        let receipt = routing.route_receipt.expect("route receipt");
        assert!(receipt.provider_id.is_none());
        assert_eq!(
            receipt.model_id.as_deref(),
            Some("openrouter/daemon-code-model")
        );
        assert_eq!(receipt.registry_revision, routing.model_snapshot_hash);
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_turn_start_rejects_stale_thread_settings_revision() {
        let workspace = temp_workspace("conversation-turn-start-stale-thread-settings");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Auto,
                    model_id: None,
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Standard,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "auto".to_string(),
                max_parallelism: 4,
                verifier_count: 1,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: None,
                updated_at_ms: 2_500,
            };
            repo.set_thread_settings(
                &conversation_id,
                &serde_json::to_string(&thread_settings).expect("settings json"),
                2_500,
            )
            .expect("set settings");
        }

        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-turn-start-stale-settings",
            "idem-conversation-turn-start-stale-settings",
        );
        turn_request.thread_settings_updated_at_ms = Some(2_499);
        let request = EngineRequest::conversation_turn_start(turn_request);
        let input = serde_json::to_string(&request).expect("request");

        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");

        assert!(accepted_lines(&output).is_empty());
        assert!(output.contains("\"event_type\":\"error\""));
        assert!(output.contains("stale thread settings revision"));
        assert!(output.contains("\"request_id\":\"req-conversation-turn-start-stale-settings\""));
        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        assert!(
            repo.lookup_turn_idempotency(
                "idem-conversation-turn-start-stale-settings",
                &conversation_id,
            )
            .expect("idempotency lookup")
            .is_none()
        );
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_turn_start_objective_planner_bootstraps_compiled_work_dag() {
        let workspace = temp_workspace("conversation-turn-start-objective-planner");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Pinned,
                    model_id: Some("provider-1/code-model".to_string()),
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Maximum,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "objective-planner".to_string(),
                max_parallelism: 5,
                verifier_count: 2,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: None,
                updated_at_ms: 1_950,
            };
            repo.set_thread_settings(
                &conversation_id,
                &serde_json::to_string(&thread_settings).expect("settings json"),
                1_950,
            )
            .expect("set objective settings");
        }
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-turn-start-objective",
            "idem-conversation-turn-start-objective",
        );
        turn_request.message.text =
            "Implement provider routing with parallel verifier proof".to_string();
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

        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted[0].run_id).expect("replay");
        let root_event = events
            .iter()
            .find(|event| {
                event.kind == EventKind::WorkItemQueued
                    && event.payload["source"] == "conversation.turn_start"
            })
            .expect("root queued event");
        assert_eq!(
            root_event.payload["objective_plan_source"],
            "objective_planner"
        );
        let safe_run_id = safe_artifact_segment(&accepted[0].run_id);
        let expected_graph_ref =
            format!("artifact://.opensks/runtime/objective-plans/{safe_run_id}/graph.json");
        let expected_compiled_plan_ref =
            format!("artifact://.opensks/runtime/objective-plans/{safe_run_id}/compiled-plan.json");
        let expected_receipt_ref =
            format!("artifact://.opensks/runtime/objective-plans/{safe_run_id}/receipt.json");
        assert_eq!(
            root_event.payload["objective_graph_ref"].as_str(),
            Some(expected_graph_ref.as_str())
        );
        assert_eq!(
            root_event.payload["objective_compiled_plan_ref"].as_str(),
            Some(expected_compiled_plan_ref.as_str())
        );
        assert_eq!(
            root_event.payload["objective_receipt_ref"].as_str(),
            Some(expected_receipt_ref.as_str())
        );
        assert_eq!(
            root_event.payload["objective_work_item_count"]
                .as_u64()
                .expect("objective count") as usize,
            events
                .iter()
                .filter(|event| event.payload["source"] == "conversation.objective_plan")
                .count()
        );
        assert!(
            root_event
                .evidence_refs
                .iter()
                .any(|evidence| { evidence == "scheduler:objective-plan-turn-bootstrap" }),
            "root event should carry objective bootstrap evidence: {root_event:#?}"
        );

        let objective_events: Vec<_> = events
            .iter()
            .filter(|event| {
                event.kind == EventKind::WorkItemQueued
                    && event.payload["source"] == "conversation.objective_plan"
            })
            .collect();
        assert!(
            !objective_events.is_empty(),
            "objective-planner turn should queue compiled DAG work items: {events:#?}"
        );
        let first_objective = objective_events[0];
        assert_eq!(
            first_objective.payload["parent_work_item_id"].as_str(),
            Some(format!("turn-root-{}", accepted[0].turn_id).as_str())
        );
        assert_eq!(
            first_objective.payload["objective_plan_source"],
            "objective_planner"
        );
        assert!(first_objective.evidence_refs.iter().any(|evidence| {
            evidence == "daemon:conversation-turn-objective-planner-bootstrap"
        }));
        assert!(
            first_objective
                .evidence_refs
                .iter()
                .any(|evidence| { evidence == "daemon:objective-plan-artifact" })
        );
        assert!(
            first_objective
                .evidence_refs
                .iter()
                .any(|evidence| { evidence == "engine:scheduler-requirement-propagation" })
        );
        assert_eq!(
            first_objective.payload["objective_receipt_ref"].as_str(),
            Some(expected_receipt_ref.as_str())
        );
        assert!(
            first_objective.payload["work_item"]["requirement_ids"]
                .as_array()
                .expect("requirement ids")
                .iter()
                .any(|id| id.as_str() == Some(accepted[0].turn_id.as_str()))
        );
        let receipt_path = workspace.join(
            expected_receipt_ref
                .strip_prefix("artifact://")
                .expect("receipt artifact ref"),
        );
        let receipt_raw = std::fs::read_to_string(&receipt_path).expect("receipt artifact");
        let receipt: opensks_contracts::ObjectivePlanReceipt =
            serde_json::from_str(&receipt_raw).expect("objective receipt");
        assert_eq!(
            receipt.graph_ref.as_deref(),
            Some(expected_graph_ref.as_str())
        );
        assert_eq!(
            receipt.compiled_plan_ref.as_deref(),
            Some(expected_compiled_plan_ref.as_str())
        );
        assert!(
            !receipt_raw.contains("Implement provider routing with parallel verifier proof"),
            "objective receipt must carry hashes/refs, not raw prompt text"
        );
        assert!(
            workspace
                .join(
                    expected_graph_ref
                        .strip_prefix("artifact://")
                        .expect("graph artifact ref")
                )
                .is_file()
        );
        assert!(
            workspace
                .join(
                    expected_compiled_plan_ref
                        .strip_prefix("artifact://")
                        .expect("compiled artifact ref")
                )
                .is_file()
        );
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn conversation_turn_start_objective_planner_uses_live_planner_model_directive() {
        let workspace = temp_workspace("conversation-turn-start-objective-live-planner");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let (endpoint, _max_active_role_calls) =
            spawn_chat_completion_server("ROUTED_NOTE.md", "live planner unused");
        seed_healthy_provider_model_with_endpoint(&workspace, &endpoint, true);
        {
            let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
            let thread_settings = ConversationThreadSettings {
                schema: CONVERSATION_THREAD_SETTINGS_SCHEMA.to_string(),
                conversation_id: conversation_id.clone(),
                model_selection: ModelSelection {
                    mode: ModelSelectionMode::Pinned,
                    model_id: Some("provider-1/code-model".to_string()),
                    fallback_model_ids: Vec::new(),
                },
                reasoning_effort: ReasoningEffort::Maximum,
                execution_mode: ExecutionMode::Worktree,
                pipeline_id: "objective-planner".to_string(),
                max_parallelism: 6,
                verifier_count: 2,
                tool_policy_id: "project-default".to_string(),
                approval_policy_id: "safe-interactive".to_string(),
                token_budget: None,
                cost_budget_usd: None,
                timeout_ms: None,
                image_model_id: None,
                updated_at_ms: 1_950,
            };
            repo.set_thread_settings(
                &conversation_id,
                &serde_json::to_string(&thread_settings).expect("settings json"),
                1_950,
            )
            .expect("set objective settings");
        }
        let mut turn_request = turn_start_request(
            &project_id,
            &conversation_id,
            "req-conversation-turn-start-objective-live",
            "idem-conversation-turn-start-objective-live",
        );
        turn_request.message.text = "Plan provider runtime with live planner proof".to_string();
        let request = EngineRequest::conversation_turn_start(turn_request);
        let output = run_stdio(
            &(serde_json::to_string(&request).expect("request") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(!output.contains("fixture-credential"));
        let accepted = accepted_lines(&output);
        assert_eq!(accepted.len(), 1);

        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store.replay(&accepted[0].run_id).expect("replay");
        let root_event = events
            .iter()
            .find(|event| {
                event.kind == EventKind::WorkItemQueued
                    && event.payload["source"] == "conversation.turn_start"
            })
            .expect("root queued event");
        assert_eq!(
            root_event.payload["objective_plan_source"],
            "model_authored_objective_planner"
        );
        assert!(
            root_event
                .evidence_refs
                .iter()
                .any(|evidence| { evidence == "daemon:objective-plan-live-model-planner" })
        );
        let safe_run_id = safe_artifact_segment(&accepted[0].run_id);
        let receipt_path = workspace
            .join(".opensks")
            .join("runtime")
            .join("objective-plans")
            .join(safe_run_id)
            .join("receipt.json");
        let receipt_raw = std::fs::read_to_string(&receipt_path).expect("receipt artifact");
        assert!(
            !receipt_raw.contains("Plan provider runtime with live planner proof"),
            "objective receipt must not persist raw objective text"
        );
        assert!(!receipt_raw.contains("live planner unused"));
        let receipt: opensks_contracts::ObjectivePlanReceipt =
            serde_json::from_str(&receipt_raw).expect("objective receipt");
        assert_eq!(receipt.source, "model_authored_objective_planner");
        assert_eq!(receipt.max_parallelism, 5);
        assert_eq!(receipt.role_count, 5);
        assert_eq!(receipt.planner_provider_id.as_deref(), Some("provider-1"));
        assert_eq!(
            receipt.planner_model_id.as_deref(),
            Some("provider-1/code-model")
        );
        assert!(receipt.planner_response_hash.is_some());
        assert!(receipt.planner_response_bytes.unwrap_or_default() > 0);
        assert!(
            receipt
                .evidence_refs
                .iter()
                .any(|evidence| { evidence == "daemon:objective-plan-live-model-planner" })
        );
        assert!(
            receipt
                .evidence_refs
                .iter()
                .any(|evidence| { evidence == "provider:role-routing" })
        );
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
        let store =
            opensks_event_store::EventStore::open_workspace(&workspace).expect("event store");
        let events = store
            .replay(&accepted[0].run_id)
            .expect("replay scheduler bootstrap events");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == EventKind::WorkItemQueued)
                .count(),
            1,
            "idempotent replay must not duplicate scheduler root work items: {events:#?}"
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
    fn run_start_request_can_use_objective_planner_graph() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-daemon-objective-planner-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut request = EngineRequest::run_start(
            "req-objective-plan",
            "objective-planner",
            "Implement provider UI with image proof",
        );
        request.params.run_id = Some("run-daemon-objective-plan".to_string());
        let input = serde_json::to_string(&request).expect("request");
        let output = run_stdio(
            &(input + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("stdio");
        assert!(output.contains("\"graph_source\":\"objective_planner\""));
        assert!(output.contains("\"pipeline_id\":\"objective-plan-"));
        assert!(output.contains("\"daemon:objective-planner-run-start\""));
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
        assert!(output.contains("\"frame_type\":\"stream_opened\""));
        assert!(output.contains("\"frame_type\":\"event\""));
        assert!(output.contains("\"frame_type\":\"stream_completed\""));
        assert!(output.contains("\"reason_code\":\"replay_complete\""));
        assert!(output.contains("\"stream_id\":\"event-stream-run-daemon-replay\""));
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
        let mut subscribe = EngineRequest::subscribe_events("req-tail", "run-daemon-tail", Some(0));
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
        assert!(output.contains("event stream replayed"));
        assert!(output.contains("event stream tail completed"));
        assert!(output.contains("\"daemon:subscription-tail-complete\""));
        assert!(output.contains("\"event-store:poll-tail\""));
        assert!(output.contains("\"daemon:poll-interval-ms:10\""));
        assert!(output.contains("\"frame_type\":\"stream_completed\""));
        assert!(output.contains("\"reason_code\":\"tail_complete\""));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn subscribe_events_reports_cursor_gap_for_out_of_range_reconnect() {
        let workspace = std::env::temp_dir().join(format!(
            "opensks-daemon-subscribe-gap-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut start = EngineRequest::run_start("req-run", "single-model-safe", "run daemon gap");
        start.params.run_id = Some("run-daemon-gap".to_string());
        let mut subscribe = EngineRequest::subscribe_events("req-gap", "run-daemon-gap", Some(999));
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
        assert!(output.contains("\"request_id\":\"req-gap\""));
        assert!(output.contains("event stream cursor gap"));
        assert!(output.contains("\"daemon:subscription-cursor-gap\""));
        assert!(output.contains("\"event-store:last-sequence\""));
        assert!(output.contains("\"frame_type\":\"stream_opened\""));
        assert!(output.contains("\"frame_type\":\"stream_failed\""));
        assert!(output.contains("\"code\":\"subscription_cursor_gap\""));
        assert!(output.contains("\"retryable\":true"));
        assert!(output.contains("\"resumable\":true"));
        assert!(output.contains("\"remediation\":\"Reconnect from sequence"));
        assert!(!output.contains("\"frame_type\":\"stream_completed\""));
        assert!(!output.contains("event stream tail completed"));
        assert!(!output.contains(workspace.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn subscribe_events_materializes_conversation_timeline_projection_once() {
        let workspace = temp_workspace("subscribe-conversation-timeline");
        let (project_id, conversation_id) = seed_conversation(&workspace);
        let start = EngineRequest::conversation_turn_start(turn_start_request(
            &project_id,
            &conversation_id,
            "req-subscribe-timeline-start",
            "idem-subscribe-timeline-start",
        ));
        let start_output = run_stdio(
            &(serde_json::to_string(&start).expect("start") + "\n"),
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("start stdio");
        let accepted = accepted_lines(&start_output);
        assert_eq!(accepted.len(), 1);
        let run_id = accepted[0].run_id.clone();

        let subscribe = EngineRequest::subscribe_events("req-subscribe-timeline", &run_id, Some(0));
        let subscribe_input = serde_json::to_string(&subscribe).expect("subscribe") + "\n";
        let subscribe_output = run_stdio(
            &subscribe_input,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("subscribe stdio");
        assert!(subscribe_output.contains("event stream replayed"));
        assert!(subscribe_output.contains("\"event-store:replay-since\""));
        assert!(subscribe_output.contains("\"frame_type\":\"stream_opened\""));
        assert!(subscribe_output.contains(&format!("\"stream_id\":\"{}\"", accepted[0].stream_id)));
        assert!(subscribe_output.contains(&format!("\"project_id\":\"{project_id}\"")));
        assert!(subscribe_output.contains(&format!("\"conversation_id\":\"{conversation_id}\"")));

        let repo = ConversationRepository::open_workspace(&workspace).expect("repo");
        let first_items = repo
            .timeline_items_for_conversation(&conversation_id, 20)
            .expect("timeline");
        let first_event_items: Vec<_> = first_items
            .iter()
            .filter(|item| item.id.starts_with("timeline-event-"))
            .collect();
        assert!(!first_event_items.is_empty());
        assert!(first_event_items.iter().all(|item| {
            item.run_id.as_deref() == Some(run_id.as_str())
                && item.payload["projection"] == "event_journal_replay"
        }));
        let cursor = repo
            .stream_cursor_for_run(&run_id)
            .expect("cursor query")
            .expect("stream cursor");
        assert!(cursor.last_sequence >= 1);
        repo.upsert_run_projection_with_last_sequence(
            &run_id,
            &project_id,
            &conversation_id,
            &accepted[0].turn_id,
            "failed",
            99,
            2_050,
        )
        .expect("stale projection");
        assert_eq!(
            repo.run_projection_state(&run_id)
                .expect("stale state")
                .as_deref(),
            Some("failed")
        );

        run_stdio(
            &subscribe_input,
            &DaemonOptions {
                workspace: workspace.clone(),
            },
        )
        .expect("subscribe replay stdio");
        let replay_items = repo
            .timeline_items_for_conversation(&conversation_id, 20)
            .expect("timeline replay");
        let replay_event_count = replay_items
            .iter()
            .filter(|item| item.id.starts_with("timeline-event-"))
            .count();
        assert_eq!(replay_event_count, first_event_items.len());
        assert_eq!(
            repo.run_projection_state(&run_id)
                .expect("rebuilt state")
                .as_deref(),
            Some("queued"),
            "cursor-0 subscribe replay must rebuild from the event journal instead of trusting stale projection state"
        );
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
