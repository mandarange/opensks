use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{SOURCE_ALIAS_CATALOG, SOURCE_SHELL_PREFIX, SuggestionCandidate};

const BUILT_IN_COMMANDS: &[&str] = &[
    "git status",
    "git diff",
    "git diff --stat",
    "git log --oneline -20",
    "git branch --show-current",
    "cargo fmt",
    "cargo fmt --check",
    "cargo check",
    "cargo test",
    "cargo clippy --all-targets --all-features",
    "cargo run -- terminal smoke",
    "cargo run -- provider list",
    "cargo run -- provider probe",
    "cargo run -- qa run",
    "cargo run -- codegraph index",
    "cargo run -- daemon --stdio --workspace \"$PWD\"",
];

const EXPLICIT_ALIASES: &[(&str, &str, &str)] = &[
    ("git st", "git status", "Expand common git status shorthand"),
    (
        "git stat",
        "git status",
        "Expand common git status shorthand",
    ),
    (
        "cargo t",
        "cargo test",
        "Expand common cargo test shorthand",
    ),
    (
        "cargo te",
        "cargo test",
        "Expand common cargo test shorthand",
    ),
    (
        "cargo c",
        "cargo check",
        "Expand common cargo check shorthand",
    ),
    ("cargo f", "cargo fmt", "Expand common cargo fmt shorthand"),
];

pub(crate) fn catalog_suggestions(
    workspace_root: &Path,
    catalog_path: Option<&Path>,
    input: &str,
) -> Vec<SuggestionCandidate> {
    let trimmed = input.trim_end();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut suggestions = Vec::new();
    for (alias, replacement, description) in EXPLICIT_ALIASES {
        if trimmed == *alias {
            suggestions.push(
                SuggestionCandidate::new(*replacement, *replacement, SOURCE_ALIAS_CATALOG, 0.95)
                    .with_description(*description),
            );
        }
    }

    for command in load_catalog_commands(workspace_root, catalog_path) {
        if command == trimmed || !command.starts_with(trimmed) {
            continue;
        }
        if unresolved_placeholder(&command) {
            continue;
        }
        suggestions.push(
            SuggestionCandidate::new(command.clone(), command, SOURCE_ALIAS_CATALOG, 0.86)
                .with_description("Command catalog prefix match"),
        );
    }
    suggestions
}

pub(crate) fn shell_prefix_suggestions(input: &str) -> Vec<SuggestionCandidate> {
    let trimmed = input.trim_end();
    if trimmed.is_empty() {
        return Vec::new();
    }
    BUILT_IN_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.starts_with(trimmed) && *command != trimmed)
        .filter(|command| !unresolved_placeholder(command))
        .map(|command| {
            SuggestionCandidate::new(command, command, SOURCE_SHELL_PREFIX, 0.78)
                .with_description("Shell-like command prefix")
        })
        .collect()
}

pub(crate) fn load_catalog_commands(
    workspace_root: &Path,
    catalog_path: Option<&Path>,
) -> Vec<String> {
    let path = catalog_path.map(PathBuf::from).unwrap_or_else(|| {
        workspace_root.join(".opensks/wiki/records/terminal-command-catalog.md")
    });
    let commands = fs::read_to_string(path)
        .ok()
        .map(|content| parse_markdown_catalog(&content))
        .filter(|commands| !commands.is_empty())
        .unwrap_or_else(|| {
            BUILT_IN_COMMANDS
                .iter()
                .map(|command| (*command).to_string())
                .collect()
        });
    commands
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_markdown_catalog(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.contains(" require ") && !line.contains(" must "))
        .map(str::to_string)
        .collect()
}

fn unresolved_placeholder(command: &str) -> bool {
    command.contains("<package>") || command.contains("<text>") || command.contains("<path>")
}
