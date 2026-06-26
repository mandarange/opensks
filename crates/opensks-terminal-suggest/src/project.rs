use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{SOURCE_PROJECT_CATALOG, SuggestionCandidate};

pub(crate) fn project_suggestions(workspace_root: &Path, input: &str) -> Vec<SuggestionCandidate> {
    let mut suggestions = Vec::new();
    suggestions.extend(cargo_package_suggestions(workspace_root, input));
    suggestions.extend(project_command_suggestions(workspace_root, input));
    suggestions
}

pub(crate) fn cargo_workspace_packages(workspace_root: &Path) -> Vec<String> {
    let mut packages = BTreeSet::new();
    let root_manifest = workspace_root.join("Cargo.toml");
    for member in cargo_members(&root_manifest) {
        let manifest = if member.as_os_str() == "." {
            root_manifest.clone()
        } else {
            workspace_root.join(member).join("Cargo.toml")
        };
        if let Some(name) = package_name(&manifest) {
            packages.insert(name);
        }
    }
    if packages.is_empty() {
        if let Some(name) = package_name(&root_manifest) {
            packages.insert(name);
        }
    }
    packages.into_iter().collect()
}

fn cargo_package_suggestions(workspace_root: &Path, input: &str) -> Vec<SuggestionCandidate> {
    let prefixes = [
        "cargo test -p ",
        "cargo check -p ",
        "cargo clippy -p ",
        "cargo run -p ",
    ];
    let Some(prefix) = prefixes.iter().find(|prefix| input.starts_with(**prefix)) else {
        return Vec::new();
    };
    let partial = input[prefix.len()..].trim();
    cargo_workspace_packages(workspace_root)
        .into_iter()
        .filter(|package| package.starts_with(partial))
        .take(20)
        .map(|package| {
            let replacement = format!("{prefix}{package}");
            SuggestionCandidate::new(
                replacement.clone(),
                replacement,
                SOURCE_PROJECT_CATALOG,
                0.90,
            )
            .with_description("Cargo workspace package")
        })
        .collect()
}

fn project_command_suggestions(workspace_root: &Path, input: &str) -> Vec<SuggestionCandidate> {
    let mut commands = Vec::new();
    if workspace_root.join("Cargo.toml").exists() {
        commands.extend([
            "cargo fmt".to_string(),
            "cargo fmt --check".to_string(),
            "cargo check".to_string(),
            "cargo test".to_string(),
            "cargo clippy --all-targets --all-features".to_string(),
        ]);
    }
    commands.extend(package_json_commands(workspace_root));
    commands.extend(makefile_commands(workspace_root));
    commands.extend(justfile_commands(workspace_root));
    commands.extend(taskfile_commands(workspace_root));

    commands
        .into_iter()
        .filter(|command| command.starts_with(input.trim_end()) && *command != input.trim_end())
        .map(|command| {
            SuggestionCandidate::new(command.clone(), command, SOURCE_PROJECT_CATALOG, 0.88)
                .with_description("Project command")
        })
        .collect()
}

fn cargo_members(root_manifest: &Path) -> Vec<PathBuf> {
    let Ok(content) = fs::read_to_string(root_manifest) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    let mut in_members = false;
    for line in content.lines() {
        let stripped = line.trim();
        if stripped.starts_with("members") && stripped.contains('[') {
            in_members = true;
        }
        if in_members {
            members.extend(quoted_values(stripped).into_iter().map(PathBuf::from));
            if stripped.contains(']') {
                break;
            }
        }
    }
    members
}

fn package_name(manifest: &Path) -> Option<String> {
    let content = fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in content.lines() {
        let stripped = line.trim();
        if stripped == "[package]" {
            in_package = true;
            continue;
        }
        if stripped.starts_with('[') && stripped != "[package]" {
            in_package = false;
        }
        if in_package && stripped.starts_with("name") {
            return quoted_values(stripped).into_iter().next();
        }
    }
    None
}

fn quoted_values(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('"') else {
            break;
        };
        values.push(after_start[..end].to_string());
        rest = &after_start[end + 1..];
    }
    values
}

fn package_json_commands(workspace_root: &Path) -> Vec<String> {
    let manifest = workspace_root.join("package.json");
    if !manifest.exists() {
        return Vec::new();
    }
    let runner = if workspace_root.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if workspace_root.join("yarn.lock").exists() {
        "yarn"
    } else {
        "npm"
    };
    let Ok(content) = fs::read_to_string(manifest) else {
        return Vec::new();
    };
    let scripts = parse_package_scripts(&content);
    scripts
        .into_iter()
        .map(|script| match runner {
            "pnpm" => format!("pnpm run {script}"),
            "yarn" => format!("yarn {script}"),
            _ => format!("npm run {script}"),
        })
        .collect()
}

fn parse_package_scripts(content: &str) -> Vec<String> {
    let mut scripts = Vec::new();
    let mut in_scripts = false;
    for line in content.lines() {
        let stripped = line.trim();
        if stripped.starts_with("\"scripts\"") && stripped.contains('{') {
            in_scripts = true;
            continue;
        }
        if in_scripts && stripped.starts_with('}') {
            break;
        }
        if in_scripts {
            if let Some(name) = quoted_values(stripped).into_iter().next() {
                scripts.push(name);
            }
        }
    }
    scripts
}

fn makefile_commands(workspace_root: &Path) -> Vec<String> {
    let path = workspace_root.join("Makefile");
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| line.split_once(':').map(|(target, _)| target.trim()))
        .filter(|target| {
            !target.is_empty()
                && !target.starts_with('.')
                && target
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        })
        .map(|target| format!("make {target}"))
        .collect()
}

fn justfile_commands(workspace_root: &Path) -> Vec<String> {
    let path = workspace_root.join("justfile");
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| line.split_once(':').map(|(target, _)| target.trim()))
        .filter(|target| !target.is_empty() && !target.starts_with('#'))
        .map(|target| target.split_whitespace().next().unwrap_or(target))
        .map(|target| format!("just {target}"))
        .collect()
}

fn taskfile_commands(workspace_root: &Path) -> Vec<String> {
    let path = workspace_root.join("Taskfile.yml");
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut in_tasks = false;
    let mut commands = Vec::new();
    for line in content.lines() {
        if line.trim() == "tasks:" {
            in_tasks = true;
            continue;
        }
        if in_tasks {
            if !line.starts_with("  ") && !line.trim().is_empty() {
                break;
            }
            let stripped = line.trim();
            if let Some(task) = stripped.strip_suffix(':') {
                if !task.is_empty() {
                    commands.push(format!("task {task}"));
                }
            }
        }
    }
    commands
}
