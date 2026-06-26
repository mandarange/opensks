use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::{SOURCE_PATH_COMPLETION, SuggestionCandidate};

const PATH_COMMANDS: &[&str] = &["cd", "cat", "open", "code", "vim", "nvim", "ls"];
const EXCLUDED_NAMES: &[&str] = &[".git", "target", "node_modules"];

pub(crate) fn path_suggestions(
    workspace_root: &Path,
    cwd: &Path,
    input: &str,
) -> Vec<SuggestionCandidate> {
    let Some((command, prefix)) = path_prefix(input) else {
        return Vec::new();
    };
    let (base, file_prefix, display_parent) = split_prefix(cwd, prefix);
    if !inside_workspace(workspace_root, &base) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&base) else {
        return Vec::new();
    };

    let allow_hidden = file_prefix.starts_with('.');
    let mut results = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_entry(workspace_root, &entry.path(), &name, allow_hidden) {
            continue;
        }
        if !name.starts_with(&file_prefix) {
            continue;
        }
        let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        let mut completed = join_display_path(&display_parent, &name);
        if is_dir {
            completed.push('/');
        }
        let replacement = format!("{command} {completed}");
        results.push(
            SuggestionCandidate::new(
                replacement.clone(),
                replacement,
                SOURCE_PATH_COMPLETION,
                0.80,
            )
            .with_description("Workspace path completion"),
        );
    }
    results.sort_by(|left, right| left.replacement.cmp(&right.replacement));
    results.truncate(20);
    results
}

fn path_prefix(input: &str) -> Option<(&str, &str)> {
    let trimmed = input.trim_end();
    let (command, rest) = trimmed.split_once(' ')?;
    if !PATH_COMMANDS.contains(&command) || rest.contains(' ') {
        return None;
    }
    Some((command, rest))
}

fn split_prefix(cwd: &Path, prefix: &str) -> (PathBuf, String, String) {
    let raw = PathBuf::from(prefix);
    let (parent, file_prefix) = if prefix.ends_with('/') {
        (raw, String::new())
    } else {
        let parent = raw.parent().map(Path::to_path_buf).unwrap_or_default();
        let file_prefix = raw
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| prefix.to_string());
        (parent, file_prefix)
    };
    let base = if parent.as_os_str().is_empty() {
        cwd.to_path_buf()
    } else if parent.is_absolute() {
        parent.clone()
    } else {
        cwd.join(&parent)
    };
    let display_parent = if prefix.ends_with('/') {
        prefix.to_string()
    } else {
        parent.to_string_lossy().trim_end_matches('/').to_string()
    };
    (base, file_prefix, display_parent)
}

fn should_skip_entry(workspace_root: &Path, path: &Path, name: &str, allow_hidden: bool) -> bool {
    if name.starts_with('.') && !allow_hidden {
        return true;
    }
    if EXCLUDED_NAMES.contains(&name) {
        return true;
    }
    let relative = path.strip_prefix(workspace_root).unwrap_or(path);
    let mut components = relative.components().filter_map(component_name);
    let first = components.next();
    let second = components.next();
    matches!(
        (first.as_deref(), second.as_deref()),
        (Some(".opensks"), Some("runtime" | "logs"))
    )
}

fn component_name(component: Component<'_>) -> Option<String> {
    component.as_os_str().to_str().map(str::to_string)
}

fn join_display_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

fn inside_workspace(workspace_root: &Path, path: &Path) -> bool {
    let Ok(root) = workspace_root.canonicalize() else {
        return false;
    };
    let Ok(path) = path.canonicalize() else {
        return false;
    };
    path.starts_with(root)
}
