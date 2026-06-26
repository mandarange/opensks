use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{SOURCE_HISTORY, SuggestionCandidate, looks_secret_like};

pub(crate) fn history_suggestions(workspace_root: &Path, input: &str) -> Vec<SuggestionCandidate> {
    let mut commands = BTreeSet::new();
    for path in history_paths(workspace_root) {
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines() {
            if let Some(command) = command_from_jsonl(line) {
                if command.starts_with(input) && command != input && !looks_secret_like(&command) {
                    commands.insert(command);
                }
            }
        }
    }
    commands
        .into_iter()
        .take(20)
        .map(|command| {
            SuggestionCandidate::new(command.clone(), command, SOURCE_HISTORY, 0.75)
                .with_description("OpenSKS terminal history")
        })
        .collect()
}

fn history_paths(workspace_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let cache = workspace_root.join(".opensks/runtime/terminal/suggestions/cache.jsonl");
    if cache.exists() {
        paths.push(cache);
    }
    let sessions = workspace_root.join(".opensks/runtime/terminal/sessions");
    let Ok(entries) = fs::read_dir(sessions) else {
        return paths;
    };
    for entry in entries.flatten() {
        let blocks = entry.path().join("blocks.jsonl");
        if blocks.exists() {
            paths.push(blocks);
        }
    }
    paths
}

fn command_from_jsonl(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    for key in [
        "redacted_command",
        "command_redacted",
        "command",
        "input",
        "replacement",
    ] {
        if let Some(command) = value.get(key).and_then(Value::as_str) {
            let command = command.trim();
            if !command.is_empty() {
                return Some(command.to_string());
            }
        }
    }
    value
        .get("summary")
        .and_then(|summary| summary.get("command"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
}
