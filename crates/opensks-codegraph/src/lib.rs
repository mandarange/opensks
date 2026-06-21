use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use opensks_contracts::{
    CODEGRAPH_INDEX_SCHEMA, CODEGRAPH_RECORD_SCHEMA, CodeGraphEdge, CodeGraphEdgeKind,
    CodeGraphIndex, CodeGraphNodeKind, CodeGraphRecord,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodeGraphError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default)]
pub struct CodeGraph {
    records: BTreeMap<String, CodeGraphRecord>,
    edges: Vec<CodeGraphEdge>,
}

impl CodeGraph {
    pub fn index_workspace(workspace: &Path) -> Result<Self, CodeGraphError> {
        let mut graph = Self::default();
        for path in collect_source_files(workspace)? {
            graph.update_file(workspace, &path)?;
        }
        Ok(graph)
    }

    pub fn update_file(&mut self, workspace: &Path, path: &Path) -> Result<(), CodeGraphError> {
        let relative = relative_path(workspace, path);
        self.delete_path(&relative);
        let content = fs::read_to_string(path)?;
        let hash = stable_hash(content.as_bytes());
        let file_id = format!("file:{relative}");
        self.records.insert(
            file_id.clone(),
            CodeGraphRecord {
                schema: CODEGRAPH_RECORD_SCHEMA.to_string(),
                id: file_id.clone(),
                kind: CodeGraphNodeKind::File,
                path: relative.clone(),
                name: path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                line: 0,
                content_hash: hash.clone(),
                evidence_refs: vec!["opensks-codegraph:file-scan".to_string()],
            },
        );
        for record in parse_records(&relative, &content, &hash) {
            self.edges.push(CodeGraphEdge {
                from_id: file_id.clone(),
                to_id: record.id.clone(),
                kind: CodeGraphEdgeKind::Contains,
            });
            self.records.insert(record.id.clone(), record);
        }
        Ok(())
    }

    pub fn delete_path(&mut self, relative: &str) {
        let prefix = format!("{relative}:");
        self.records
            .retain(|_, record| record.path != relative && !record.id.contains(&prefix));
        self.edges.retain(|edge| {
            self.records.contains_key(&edge.from_id) && self.records.contains_key(&edge.to_id)
        });
    }

    pub fn query(&self, text: &str) -> Vec<CodeGraphRecord> {
        let needle = text.to_ascii_lowercase();
        self.records
            .values()
            .filter(|record| {
                record.name.to_ascii_lowercase().contains(&needle)
                    || record.path.to_ascii_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    }

    pub fn to_index(&self) -> CodeGraphIndex {
        let mut records: Vec<_> = self.records.values().cloned().collect();
        records.sort_by(|left, right| left.id.cmp(&right.id));
        let mut edges = self.edges.clone();
        edges.sort_by(|left, right| {
            left.from_id
                .cmp(&right.from_id)
                .then_with(|| left.to_id.cmp(&right.to_id))
        });
        let fingerprint = stable_hash(
            records
                .iter()
                .map(|record| record.content_hash.as_str())
                .collect::<Vec<_>>()
                .join("|")
                .as_bytes(),
        );
        CodeGraphIndex {
            schema: CODEGRAPH_INDEX_SCHEMA.to_string(),
            workspace_fingerprint: fingerprint,
            records,
            edges,
            freshness: "fresh".to_string(),
        }
    }
}

pub fn write_index(workspace: &Path, graph: &CodeGraph) -> Result<PathBuf, CodeGraphError> {
    let path = workspace
        .join(".opensks")
        .join("wiki")
        .join("indexes")
        .join("codegraph.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &path,
        serde_json::to_string_pretty(&graph.to_index())? + "\n",
    )?;
    Ok(path)
}

fn collect_source_files(workspace: &Path) -> Result<Vec<PathBuf>, CodeGraphError> {
    let mut files = Vec::new();
    collect_dir(workspace, workspace, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_dir(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), CodeGraphError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                ".git"
                    | ".opensks"
                    | ".sneakoscope"
                    | ".omc"
                    | ".github"
                    | "target"
                    | "node_modules"
                    | ".build"
                    | "runtime"
                    | "schemas"
            ) || path.starts_with(root.join(".opensks").join("runtime"))
            {
                continue;
            }
            collect_dir(root, &path, files)?;
        } else if is_supported_source(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_supported_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("rs" | "swift" | "ts" | "tsx" | "js")
    )
}

fn parse_records(relative: &str, content: &str, hash: &str) -> Vec<CodeGraphRecord> {
    let mut records = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line_no = index as u32 + 1;
        let trimmed = line.trim();
        if let Some(name) = parse_import(trimmed) {
            records.push(record(
                relative,
                CodeGraphNodeKind::Import,
                name,
                line_no,
                hash,
            ));
        }
        if let Some(name) = parse_symbol(trimmed) {
            let kind = if name.to_ascii_lowercase().contains("test") || trimmed.contains("#[test]")
            {
                CodeGraphNodeKind::Test
            } else {
                CodeGraphNodeKind::Symbol
            };
            records.push(record(relative, kind, name, line_no, hash));
        }
    }
    records
}

fn parse_import(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("use ") {
        return Some(rest.trim_end_matches(';').to_string());
    }
    if let Some(rest) = line.strip_prefix("import ") {
        return Some(rest.trim_end_matches(';').to_string());
    }
    None
}

fn parse_symbol(line: &str) -> Option<String> {
    for prefix in [
        "pub fn ",
        "fn ",
        "pub struct ",
        "struct ",
        "pub enum ",
        "enum ",
        "func ",
        "class ",
        "export function ",
        "function ",
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(
                rest.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'))
                    .next()
                    .unwrap_or("")
                    .to_string(),
            )
            .filter(|value| !value.is_empty());
        }
    }
    None
}

fn record(
    relative: &str,
    kind: CodeGraphNodeKind,
    name: String,
    line: u32,
    hash: &str,
) -> CodeGraphRecord {
    CodeGraphRecord {
        schema: CODEGRAPH_RECORD_SCHEMA.to_string(),
        id: format!("{relative}:{line}:{name}"),
        kind,
        path: relative.to_string(),
        name,
        line,
        content_hash: hash.to_string(),
        evidence_refs: vec!["opensks-codegraph:line-parser".to_string()],
    }
}

fn relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("opensks-codegraph-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("workspace");
        root
    }

    #[test]
    fn indexes_rust_swift_and_typescript_fixture() {
        let root = temp_workspace("multi");
        fs::write(
            root.join("src/lib.rs"),
            "use std::fs;\npub fn rust_symbol() {}\n#[test]\nfn rust_test() {}\n",
        )
        .expect("rust");
        fs::write(
            root.join("src/App.swift"),
            "import SwiftUI\nstruct Studio {}\nfunc swiftSymbol() {}\n",
        )
        .expect("swift");
        fs::write(
            root.join("src/app.ts"),
            "import x from 'x';\nexport function tsSymbol() {}\nclass Route {}\n",
        )
        .expect("ts");

        let graph = CodeGraph::index_workspace(&root).expect("index");
        assert!(!graph.query("rust_symbol").is_empty());
        assert!(!graph.query("SwiftUI").is_empty());
        assert!(!graph.query("tsSymbol").is_empty());
        assert!(
            graph
                .to_index()
                .records
                .iter()
                .any(|record| record.kind == CodeGraphNodeKind::Test)
        );
    }

    #[test]
    fn one_file_incremental_update_changes_query_results() {
        let root = temp_workspace("incremental");
        let path = root.join("src/lib.rs");
        fs::write(&path, "pub fn before_name() {}\n").expect("before");
        let mut graph = CodeGraph::index_workspace(&root).expect("index");
        assert_eq!(graph.query("before_name").len(), 1);
        fs::write(&path, "pub fn after_name() {}\n").expect("after");
        graph.update_file(&root, &path).expect("update");
        assert!(graph.query("before_name").is_empty());
        assert_eq!(graph.query("after_name").len(), 1);
    }

    #[test]
    fn rename_delete_removes_old_records() {
        let root = temp_workspace("delete");
        let path = root.join("src/lib.rs");
        fs::write(&path, "pub fn gone() {}\n").expect("file");
        let mut graph = CodeGraph::index_workspace(&root).expect("index");
        graph.delete_path("src/lib.rs");
        assert!(graph.query("gone").is_empty());
    }
}
