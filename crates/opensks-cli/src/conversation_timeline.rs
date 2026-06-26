use std::path::Path;

use crate::CliError;
use opensks_conversation::ConversationError;

pub(crate) fn conversation_timeline_items(
    repo: &opensks_conversation::ConversationRepository,
    workspace: &Path,
    conversation: &str,
    limit: usize,
) -> Result<Vec<opensks_contracts::TimelineItem>, CliError> {
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

    if let Some(store) = event_store.as_ref() {
        for run in runs {
            let events = store
                .replay(&run.run_id)
                .map_err(|error| CliError::Invalid(format!("replay event journal: {error}")))?;
            match repo.project_execution_events_into_timeline(&run.run_id, &events, now_ms()) {
                Ok(_) => {}
                Err(error) if should_skip_projection_error(&error) => continue,
                Err(error) => {
                    return Err(CliError::Invalid(format!(
                        "project event journal timeline: {error}"
                    )));
                }
            }
        }
    }

    repo.timeline_items_for_conversation(conversation, limit)
        .map_err(|error| CliError::Invalid(error.to_string()))
}

fn should_skip_projection_error(error: &ConversationError) -> bool {
    matches!(error, ConversationError::ProjectionMissing(_))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_projection_skips_only_stale_projection_gaps() {
        assert!(should_skip_projection_error(
            &ConversationError::ProjectionMissing("run-stale".to_string())
        ));
        assert!(!should_skip_projection_error(&ConversationError::NotFound(
            "conversation-missing".to_string()
        )));
    }
}
