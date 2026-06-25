use std::path::Path;

use crate::CliError;

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
            repo.project_execution_events_into_timeline(&run.run_id, &events, now_ms())
                .map_err(|error| {
                    CliError::Invalid(format!("project event journal timeline: {error}"))
                })?;
        }
    }

    repo.timeline_items_for_conversation(conversation, limit)
        .map_err(|error| CliError::Invalid(error.to_string()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
