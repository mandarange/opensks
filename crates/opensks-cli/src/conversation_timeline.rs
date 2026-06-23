use std::path::Path;

use crate::CliError;

const CONVERSATION_TIMELINE_SEQUENCE_STRIDE: i64 = 1_000_000;

pub(crate) fn conversation_timeline_items(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    conversation: &str,
    limit: usize,
) -> Result<Vec<opensks_contracts::TimelineItem>, CliError> {
    let mut message_items = repo
        .timeline_items_for_conversation(conversation, limit)
        .map_err(|error| CliError::Invalid(error.to_string()))?;
    let runs = repo
        .runs_for_conversation(conversation)
        .map_err(|error| CliError::Invalid(error.to_string()))?;
    let event_db = workspace.join(opensks_event_store::ENGINE_DB_RELATIVE_PATH);
    let event_store = if event_db.exists() {
        Some(
            opensks_event_store::EventStore::open_workspace(workspace)
                .map_err(|error| CliError::Invalid(format!("open event journal: {error}")))?,
        )
    } else {
        None
    };

    for item in &mut message_items {
        item.sequence = item
            .sequence
            .saturating_mul(CONVERSATION_TIMELINE_SEQUENCE_STRIDE);
    }

    let mut projected = Vec::new();
    for item in message_items {
        let anchor = item.clone();
        projected.push(item);
        let Some(run_id) = anchor.run_id.as_deref() else {
            continue;
        };
        if !runs.iter().any(|run| run.run_id == run_id) {
            continue;
        }
        let Some(store) = event_store.as_ref() else {
            continue;
        };
        let events = store
            .replay(run_id)
            .map_err(|error| CliError::Invalid(format!("replay event journal: {error}")))?;
        projected.extend(
            events
                .into_iter()
                .map(|event| execution_event_timeline_item(&anchor, event)),
        );
    }
    Ok(projected)
}

fn execution_event_timeline_item(
    anchor: &opensks_contracts::TimelineItem,
    event: opensks_contracts::ExecutionEventEnvelope,
) -> opensks_contracts::TimelineItem {
    let sequence_offset = i64::try_from(event.sequence)
        .unwrap_or(CONVERSATION_TIMELINE_SEQUENCE_STRIDE - 1)
        .min(CONVERSATION_TIMELINE_SEQUENCE_STRIDE - 1);
    let occurred_at_ms =
        execution_event_occurred_at_ms(&event.occurred_at).unwrap_or(anchor.updated_at_ms);
    let event_kind = event.kind.as_str().to_string();
    let content_redacted = execution_event_timeline_text(&event);
    opensks_contracts::TimelineItem {
        schema: opensks_contracts::TIMELINE_ITEM_SCHEMA.to_string(),
        id: format!("timeline-event-{}", event.id),
        project_id: anchor.project_id.clone(),
        conversation_id: anchor.conversation_id.clone(),
        turn_id: anchor.turn_id.clone(),
        run_id: Some(event.run_id.clone()),
        sequence: anchor.sequence.saturating_add(sequence_offset),
        kind: timeline_kind_for_execution_event(&event),
        state: event_kind.clone(),
        payload: serde_json::json!({
            "source_schema": event.schema,
            "event_id": event.id,
            "event_kind": event_kind,
            "event_sequence": event.sequence,
            "actor": event.actor,
            "causation_id": event.causation_id,
            "correlation_id": event.correlation_id,
            "content_redacted": content_redacted,
            "payload_redacted": event.payload,
            "sensitivity": event.sensitivity.as_str(),
            "evidence_refs": event.evidence_refs,
            "projection": "event_journal_replay"
        }),
        created_at_ms: occurred_at_ms,
        updated_at_ms: occurred_at_ms,
    }
}

fn timeline_kind_for_execution_event(
    event: &opensks_contracts::ExecutionEventEnvelope,
) -> opensks_contracts::TimelineItemKind {
    if let Some(agent_kind) = event
        .payload
        .get("agent_event_kind")
        .and_then(serde_json::Value::as_str)
    {
        return match agent_kind {
            "plan_updated" => opensks_contracts::TimelineItemKind::Plan,
            "tool_call_started" | "tool_call_output" | "tool_call_completed" => {
                opensks_contracts::TimelineItemKind::ToolCall
            }
            "file_patch_proposed" | "file_patch_applied" => {
                opensks_contracts::TimelineItemKind::Patch
            }
            "verification_started" | "verification_completed" => {
                opensks_contracts::TimelineItemKind::Verification
            }
            "approval_requested" | "approval_resolved" => {
                opensks_contracts::TimelineItemKind::Approval
            }
            "worker_spawned" | "worker_progress" | "worker_completed" => {
                opensks_contracts::TimelineItemKind::Worker
            }
            "image_artifact_created" => opensks_contracts::TimelineItemKind::ImageArtifact,
            "warning" => opensks_contracts::TimelineItemKind::Warning,
            "error" => opensks_contracts::TimelineItemKind::Error,
            "assistant_text_delta" | "assistant_text_completed" => {
                opensks_contracts::TimelineItemKind::AssistantMessage
            }
            _ => opensks_contracts::TimelineItemKind::Warning,
        };
    }

    match event.kind {
        opensks_contracts::EventKind::ApprovalRequested
        | opensks_contracts::EventKind::ApprovalApproved
        | opensks_contracts::EventKind::ApprovalDenied => {
            opensks_contracts::TimelineItemKind::Approval
        }
        opensks_contracts::EventKind::VerificationPassed => {
            opensks_contracts::TimelineItemKind::Verification
        }
        opensks_contracts::EventKind::VerificationFailed => {
            opensks_contracts::TimelineItemKind::Error
        }
        opensks_contracts::EventKind::WorkItemQueued
        | opensks_contracts::EventKind::WorkItemLeased
        | opensks_contracts::EventKind::WorkItemRunning
        | opensks_contracts::EventKind::WorkItemCompleted
        | opensks_contracts::EventKind::LeaseHeartbeat
        | opensks_contracts::EventKind::LeaseExpired
        | opensks_contracts::EventKind::RunStarted
        | opensks_contracts::EventKind::RunPaused
        | opensks_contracts::EventKind::RunResumed
        | opensks_contracts::EventKind::RunCancelled
        | opensks_contracts::EventKind::SteeringRequested
        | opensks_contracts::EventKind::SnapshotWritten => {
            opensks_contracts::TimelineItemKind::Worker
        }
        opensks_contracts::EventKind::Unknown => opensks_contracts::TimelineItemKind::Warning,
    }
}

fn execution_event_timeline_text(event: &opensks_contracts::ExecutionEventEnvelope) -> String {
    event
        .payload
        .get("payload")
        .and_then(|payload| payload.get("message"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            event
                .payload
                .get("message")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            event
                .payload
                .get("agent_event_kind")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_string)
        .unwrap_or_else(|| event.kind.as_str().replace('_', " "))
}

fn execution_event_occurred_at_ms(occurred_at: &str) -> Option<u64> {
    let (secs, nanos) = occurred_at.split_once('.')?;
    let secs = secs.parse::<u64>().ok()?;
    let nanos_digits: String = nanos.chars().take(9).collect();
    let nanos = nanos_digits.parse::<u64>().ok()?;
    Some(secs.saturating_mul(1_000).saturating_add(nanos / 1_000_000))
}
