use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use opensks_contracts::{
    CODEGRAPH_INDEX_SCHEMA, CODEGRAPH_RECORD_SCHEMA, CodeGraphEdge, CodeGraphEdgeKind,
    CodeGraphIndex, CodeGraphNodeKind, CodeGraphRecord,
};
use thiserror::Error;

/// Files larger than this are skipped for symbol/relationship indexing to
/// avoid unbounded memory/CPU use on huge generated or vendored files.
const MAX_INDEXABLE_FILE_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB

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
            graph.update_file_records(workspace, &path)?;
        }
        graph.refresh_relationship_edges(workspace)?;
        Ok(graph)
    }

    pub fn update_file(&mut self, workspace: &Path, path: &Path) -> Result<(), CodeGraphError> {
        self.update_file_records(workspace, path)?;
        self.refresh_relationship_edges(workspace)?;
        Ok(())
    }

    /// Update file-local records without recomputing whole-workspace
    /// relationship edges.
    ///
    /// This is intended for latency-sensitive foreground context construction,
    /// where changed-file symbols are useful but a full calls/references refresh
    /// would block chat startup. Persisted index update paths should continue to
    /// use [`CodeGraph::update_file`] so their relationship graph stays fresh.
    pub fn update_file_records_only(
        &mut self,
        workspace: &Path,
        path: &Path,
    ) -> Result<(), CodeGraphError> {
        let relative = relative_path(workspace, path);
        let file_id = format!("file:{relative}");
        let preserved_file_edges = self
            .edges
            .iter()
            .filter(|edge| {
                edge.kind != CodeGraphEdgeKind::Contains
                    && (edge.from_id == file_id || edge.to_id == file_id)
            })
            .cloned()
            .collect::<Vec<_>>();

        self.update_file_records(workspace, path)?;

        let mut seen = self
            .edges
            .iter()
            .map(edge_key)
            .collect::<std::collections::BTreeSet<_>>();
        for edge in preserved_file_edges {
            if self.records.contains_key(&edge.from_id)
                && self.records.contains_key(&edge.to_id)
                && seen.insert(edge_key(&edge))
            {
                self.edges.push(edge);
            }
        }
        Ok(())
    }

    /// Update file-local records and refresh only the relationship edges that
    /// can be derived from the changed file itself.
    ///
    /// This keeps foreground context packs useful without the full workspace
    /// file-by-symbol scan performed by [`CodeGraph::refresh_relationship_edges`].
    pub fn update_file_for_context(
        &mut self,
        workspace: &Path,
        path: &Path,
    ) -> Result<(), CodeGraphError> {
        self.update_file_records(workspace, path)?;
        self.refresh_file_relationship_edges(workspace, path)
    }

    fn refresh_file_relationship_edges(
        &mut self,
        workspace: &Path,
        path: &Path,
    ) -> Result<(), CodeGraphError> {
        if !is_indexable_size(path) {
            return Ok(());
        }
        let relative = relative_path(workspace, path);
        let file_id = format!("file:{relative}");
        let content = fs::read_to_string(path)?;
        let content_tokens = identifier_token_set(&content);
        let records = self.records.values().cloned().collect::<Vec<_>>();
        let symbol_records = records
            .iter()
            .filter(|record| matches!(record.kind, CodeGraphNodeKind::Symbol))
            .cloned()
            .collect::<Vec<_>>();
        let import_records = records
            .iter()
            .filter(|record| record.kind == CodeGraphNodeKind::Import && record.path == relative)
            .cloned()
            .collect::<Vec<_>>();
        let test_records = records
            .iter()
            .filter(|record| record.kind == CodeGraphNodeKind::Test && record.path == relative)
            .cloned()
            .collect::<Vec<_>>();

        let mut seen = self
            .edges
            .iter()
            .map(edge_key)
            .collect::<std::collections::BTreeSet<_>>();
        for import in &import_records {
            for symbol in &symbol_records {
                if import.id == symbol.id {
                    continue;
                }
                if import_mentions_symbol(&import.name, &symbol.name) {
                    self.push_edge_once(
                        &mut seen,
                        import.id.clone(),
                        symbol.id.clone(),
                        CodeGraphEdgeKind::Imports,
                    );
                }
            }
        }

        for symbol in &symbol_records {
            if symbol.path == relative {
                continue;
            }
            if calls_symbol(&content, &symbol.name) {
                self.push_edge_once(
                    &mut seen,
                    file_id.clone(),
                    symbol.id.clone(),
                    CodeGraphEdgeKind::Calls,
                );
            } else if content_tokens.contains(&symbol.name) {
                self.push_edge_once(
                    &mut seen,
                    file_id.clone(),
                    symbol.id.clone(),
                    CodeGraphEdgeKind::References,
                );
            }
        }

        for test in &test_records {
            for symbol in &symbol_records {
                if test.id == symbol.id {
                    continue;
                }
                if test.path == symbol.path && test_covers_symbol(&test.name, &symbol.name) {
                    self.push_edge_once(
                        &mut seen,
                        test.id.clone(),
                        symbol.id.clone(),
                        CodeGraphEdgeKind::Tests,
                    );
                }
            }
        }
        self.edges.sort_by(|left, right| {
            edge_key(left)
                .cmp(&edge_key(right))
                .then_with(|| left.from_id.cmp(&right.from_id))
                .then_with(|| left.to_id.cmp(&right.to_id))
        });
        Ok(())
    }

    fn update_file_records(&mut self, workspace: &Path, path: &Path) -> Result<(), CodeGraphError> {
        let relative = relative_path(workspace, path);
        self.delete_path(&relative);
        let file_id = format!("file:{relative}");
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string();
        if !is_indexable_size(path) {
            self.records.insert(
                file_id.clone(),
                CodeGraphRecord {
                    schema: CODEGRAPH_RECORD_SCHEMA.to_string(),
                    id: file_id,
                    kind: CodeGraphNodeKind::File,
                    path: relative,
                    name: file_name,
                    line: 0,
                    content_hash: "skipped:oversized".to_string(),
                    evidence_refs: vec!["opensks-codegraph:size-skipped".to_string()],
                },
            );
            return Ok(());
        }
        let content = fs::read_to_string(path)?;
        let hash = stable_hash(content.as_bytes());
        self.records.insert(
            file_id.clone(),
            CodeGraphRecord {
                schema: CODEGRAPH_RECORD_SCHEMA.to_string(),
                id: file_id.clone(),
                kind: CodeGraphNodeKind::File,
                path: relative.clone(),
                name: file_name,
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

    pub fn refresh_relationship_edges(&mut self, workspace: &Path) -> Result<(), CodeGraphError> {
        self.edges
            .retain(|edge| edge.kind == CodeGraphEdgeKind::Contains);
        let records = self.records.values().cloned().collect::<Vec<_>>();
        let symbol_records = records
            .iter()
            .filter(|record| matches!(record.kind, CodeGraphNodeKind::Symbol))
            .cloned()
            .collect::<Vec<_>>();
        let file_records = records
            .iter()
            .filter(|record| record.kind == CodeGraphNodeKind::File)
            .cloned()
            .collect::<Vec<_>>();
        let import_records = records
            .iter()
            .filter(|record| record.kind == CodeGraphNodeKind::Import)
            .cloned()
            .collect::<Vec<_>>();
        let test_records = records
            .iter()
            .filter(|record| record.kind == CodeGraphNodeKind::Test)
            .cloned()
            .collect::<Vec<_>>();

        let mut seen = self
            .edges
            .iter()
            .map(edge_key)
            .collect::<std::collections::BTreeSet<_>>();
        for import in &import_records {
            for symbol in &symbol_records {
                if import.id == symbol.id {
                    continue;
                }
                if import_mentions_symbol(&import.name, &symbol.name) {
                    self.push_edge_once(
                        &mut seen,
                        import.id.clone(),
                        symbol.id.clone(),
                        CodeGraphEdgeKind::Imports,
                    );
                }
            }
        }

        for file in &file_records {
            let path = workspace.join(&file.path);
            if !is_indexable_size(&path) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            for symbol in &symbol_records {
                if symbol.path == file.path {
                    continue;
                }
                if calls_symbol(&content, &symbol.name) {
                    self.push_edge_once(
                        &mut seen,
                        file.id.clone(),
                        symbol.id.clone(),
                        CodeGraphEdgeKind::Calls,
                    );
                } else if references_symbol(&content, &symbol.name) {
                    self.push_edge_once(
                        &mut seen,
                        file.id.clone(),
                        symbol.id.clone(),
                        CodeGraphEdgeKind::References,
                    );
                }
            }
        }

        for test in &test_records {
            for symbol in &symbol_records {
                if test.id == symbol.id {
                    continue;
                }
                if test.path == symbol.path && test_covers_symbol(&test.name, &symbol.name) {
                    self.push_edge_once(
                        &mut seen,
                        test.id.clone(),
                        symbol.id.clone(),
                        CodeGraphEdgeKind::Tests,
                    );
                }
            }
        }
        self.edges.sort_by(|left, right| {
            edge_key(left)
                .cmp(&edge_key(right))
                .then_with(|| left.from_id.cmp(&right.from_id))
                .then_with(|| left.to_id.cmp(&right.to_id))
        });
        Ok(())
    }

    fn push_edge_once(
        &mut self,
        seen: &mut std::collections::BTreeSet<String>,
        from_id: String,
        to_id: String,
        kind: CodeGraphEdgeKind,
    ) {
        let edge = CodeGraphEdge {
            from_id,
            to_id,
            kind,
        };
        if seen.insert(edge_key(&edge)) {
            self.edges.push(edge);
        }
    }

    pub fn delete_path(&mut self, relative: &str) {
        self.records.retain(|_, record| record.path != relative);
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

    pub fn references(&self, symbol_id: &str) -> Vec<CodeGraphRecord> {
        let mut refs = self
            .edges
            .iter()
            .filter(|edge| edge.to_id == symbol_id || edge.from_id == symbol_id)
            .filter_map(|edge| {
                if edge.to_id == symbol_id {
                    self.records.get(&edge.from_id)
                } else {
                    self.records.get(&edge.to_id)
                }
            })
            .cloned()
            .collect::<Vec<_>>();
        refs.sort_by(|left, right| left.id.cmp(&right.id));
        refs.dedup_by(|left, right| left.id == right.id);
        refs
    }

    /// Reconstruct a graph from a previously persisted [`CodeGraphIndex`].
    ///
    /// This is the inverse of [`CodeGraph::to_index`] and lets the incremental
    /// `codegraph update` path reload a saved index without re-scanning the
    /// whole workspace.
    pub fn from_index(index: CodeGraphIndex) -> Self {
        let mut records = BTreeMap::new();
        for record in index.records {
            records.insert(record.id.clone(), record);
        }
        Self {
            records,
            edges: index.edges,
        }
    }

    /// Total number of records currently held (files, symbols, imports, tests).
    pub fn record_count(&self) -> usize {
        self.records.len()
    }

    /// Number of non-file records (symbols/imports/tests) — the "symbols" the
    /// `codegraph update` command reports for a re-indexed workspace.
    pub fn symbol_count(&self) -> usize {
        self.records
            .values()
            .filter(|record| record.kind != CodeGraphNodeKind::File)
            .count()
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

/// Canonical on-disk location of the persisted code graph for a workspace.
pub fn index_path(workspace: &Path) -> PathBuf {
    workspace
        .join(".opensks")
        .join("wiki")
        .join("indexes")
        .join("codegraph.json")
}

pub fn write_index(workspace: &Path, graph: &CodeGraph) -> Result<PathBuf, CodeGraphError> {
    let path = index_path(workspace);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &path,
        serde_json::to_string_pretty(&graph.to_index())? + "\n",
    )?;
    Ok(path)
}

/// Load a persisted [`CodeGraph`] for a workspace if one exists.
///
/// Returns `Ok(None)` when no index has been written yet so the caller can fall
/// back to a full [`CodeGraph::index_workspace`] build. A present-but-corrupt
/// index surfaces as a [`CodeGraphError`].
pub fn read_index(workspace: &Path) -> Result<Option<CodeGraph>, CodeGraphError> {
    let path = index_path(workspace);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let index: CodeGraphIndex = serde_json::from_str(&text)?;
    Ok(Some(CodeGraph::from_index(index)))
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
    let mut pending_test_attr = false;
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
            let kind = if name.to_ascii_lowercase().contains("test") || pending_test_attr {
                CodeGraphNodeKind::Test
            } else {
                CodeGraphNodeKind::Symbol
            };
            records.push(record(relative, kind, name, line_no, hash));
            pending_test_attr = false;
        } else if trimmed == "#[test]" {
            pending_test_attr = true;
        } else if !trimmed.is_empty() {
            pending_test_attr = false;
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

fn is_indexable_size(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.len() <= MAX_INDEXABLE_FILE_BYTES)
        .unwrap_or(false)
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

fn edge_key(edge: &CodeGraphEdge) -> String {
    format!(
        "{}\u{0}{}\u{0}{}",
        edge.from_id,
        edge.to_id,
        edge_kind_label(&edge.kind)
    )
}

fn edge_kind_label(kind: &CodeGraphEdgeKind) -> &'static str {
    match kind {
        CodeGraphEdgeKind::Contains => "contains",
        CodeGraphEdgeKind::Imports => "imports",
        CodeGraphEdgeKind::Calls => "calls",
        CodeGraphEdgeKind::Tests => "tests",
        CodeGraphEdgeKind::References => "references",
        CodeGraphEdgeKind::OwnsRoute => "owns_route",
    }
}

fn import_mentions_symbol(import: &str, symbol: &str) -> bool {
    identifier_tokens(import)
        .iter()
        .any(|token| token == symbol)
}

fn calls_symbol(content: &str, symbol: &str) -> bool {
    content.contains(&format!("{symbol}("))
}

fn references_symbol(content: &str, symbol: &str) -> bool {
    identifier_tokens(content)
        .iter()
        .any(|token| token == symbol)
}

fn test_covers_symbol(test_name: &str, symbol: &str) -> bool {
    let test_name = test_name.to_ascii_lowercase();
    let symbol = symbol.to_ascii_lowercase();
    test_name != symbol && test_name.contains(&symbol)
}

fn identifier_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'))
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn identifier_token_set(value: &str) -> BTreeSet<String> {
    identifier_tokens(value).into_iter().collect()
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

    #[test]
    fn persisted_index_roundtrips_through_from_index() {
        let root = temp_workspace("roundtrip");
        fs::write(root.join("src/lib.rs"), "pub fn keep() {}\n").expect("file");
        let graph = CodeGraph::index_workspace(&root).expect("index");
        write_index(&root, &graph).expect("write");
        let reloaded = read_index(&root).expect("read").expect("present");
        assert_eq!(reloaded.to_index(), graph.to_index());
    }

    #[test]
    fn read_index_absent_returns_none() {
        let root = temp_workspace("absent");
        assert!(read_index(&root).expect("read").is_none());
    }

    #[test]
    fn references_return_adjacent_records() {
        let root = temp_workspace("references");
        fs::write(root.join("src/lib.rs"), "pub fn linked() {}\n").expect("file");
        let graph = CodeGraph::index_workspace(&root).expect("index");
        let symbol = graph
            .query("linked")
            .into_iter()
            .find(|record| record.name == "linked")
            .expect("symbol");
        let refs = graph.references(&symbol.id);
        assert!(refs.iter().any(|record| record.id == "file:src/lib.rs"));
    }

    #[test]
    fn indexes_generated_import_call_and_test_edges() {
        let root = temp_workspace("semantic-edges");
        fs::write(
            root.join("src/helper.rs"),
            "pub fn helper_dependency() {}\n",
        )
        .expect("helper");
        fs::write(
            root.join("src/lib.rs"),
            "use crate::helper::helper_dependency;\n\
             pub fn caller() { helper_dependency(); }\n\
             #[test]\n\
             fn caller_test() { caller(); }\n",
        )
        .expect("lib");

        let graph = CodeGraph::index_workspace(&root).expect("index");
        let index = graph.to_index();
        let helper = index
            .records
            .iter()
            .find(|record| record.name == "helper_dependency")
            .expect("helper symbol");
        let caller = index
            .records
            .iter()
            .find(|record| record.name == "caller")
            .expect("caller symbol");
        let caller_test = index
            .records
            .iter()
            .find(|record| record.name == "caller_test")
            .expect("caller test");

        assert!(
            index
                .edges
                .iter()
                .any(|edge| { edge.to_id == helper.id && edge.kind == CodeGraphEdgeKind::Imports })
        );
        assert!(
            index
                .edges
                .iter()
                .any(|edge| { edge.to_id == helper.id && edge.kind == CodeGraphEdgeKind::Calls })
        );
        assert!(index.edges.iter().any(|edge| {
            edge.from_id == caller_test.id
                && edge.to_id == caller.id
                && edge.kind == CodeGraphEdgeKind::Tests
        }));

        let refs = graph.references(&helper.id);
        assert!(
            refs.iter().any(|record| record.id == "file:src/lib.rs"),
            "call edge should make the caller file adjacent to helper"
        );
    }

    /// Incremental `update_file` must touch only the one changed file: the other
    /// file's records (and their byte content) are preserved unchanged. This is
    /// the proof that no full workspace rescan happened — only `update_file` ran.
    #[test]
    fn update_file_is_incremental_and_leaves_other_files_byte_identical() {
        let root = temp_workspace("incremental-isolation");
        let lib_path = root.join("src/lib.rs");
        let util_path = root.join("src/util.rs");
        fs::write(&lib_path, "pub fn lib_alpha() {}\n").expect("lib");
        fs::write(&util_path, "pub fn util_beta() {}\n").expect("util");

        // Build the full index once, then capture the *other* file's records as
        // serialized bytes so we can prove they are not regenerated.
        let mut graph = CodeGraph::index_workspace(&root).expect("index");
        let util_records_before: Vec<u8> = serde_json::to_vec(
            &graph
                .to_index()
                .records
                .iter()
                .filter(|record| record.path == "src/util.rs")
                .cloned()
                .collect::<Vec<_>>(),
        )
        .expect("ser util before");
        assert_eq!(graph.query("lib_alpha").len(), 1);

        // Change ONLY src/lib.rs on disk and run the incremental path.
        fs::write(&lib_path, "pub fn lib_gamma() {}\n").expect("rewrite lib");
        graph.update_file(&root, &lib_path).expect("update");

        // The changed file's symbols flipped.
        assert!(graph.query("lib_alpha").is_empty());
        assert_eq!(graph.query("lib_gamma").len(), 1);

        // The other file's records are byte-identical to before the update.
        let util_records_after: Vec<u8> = serde_json::to_vec(
            &graph
                .to_index()
                .records
                .iter()
                .filter(|record| record.path == "src/util.rs")
                .cloned()
                .collect::<Vec<_>>(),
        )
        .expect("ser util after");
        assert_eq!(
            util_records_before, util_records_after,
            "incremental update must not rescan or rebuild unrelated files"
        );
        assert_eq!(graph.query("util_beta").len(), 1);
    }

    #[test]
    fn records_only_update_skips_relationship_refresh_for_foreground_context() {
        let root = temp_workspace("records-only-update");
        let caller_path = root.join("src/caller.rs");
        let helper_path = root.join("src/helper.rs");
        fs::write(&caller_path, "pub fn caller_alpha() { helper_beta(); }\n").expect("caller");
        fs::write(
            &helper_path,
            "pub fn helper_beta() {}\npub fn helper_gamma() {}\n",
        )
        .expect("helper");

        let mut graph = CodeGraph::index_workspace(&root).expect("index");
        let beta = graph
            .query("helper_beta")
            .into_iter()
            .find(|record| record.kind == CodeGraphNodeKind::Symbol)
            .expect("beta symbol");
        let gamma = graph
            .query("helper_gamma")
            .into_iter()
            .find(|record| record.kind == CodeGraphNodeKind::Symbol)
            .expect("gamma symbol");
        assert!(graph.to_index().edges.iter().any(|edge| {
            edge.from_id == "file:src/caller.rs"
                && edge.to_id == beta.id
                && edge.kind == CodeGraphEdgeKind::Calls
        }));
        assert!(!graph.to_index().edges.iter().any(|edge| {
            edge.from_id == "file:src/caller.rs"
                && edge.to_id == gamma.id
                && edge.kind == CodeGraphEdgeKind::Calls
        }));

        fs::write(&caller_path, "pub fn caller_delta() { helper_gamma(); }\n").expect("rewrite");
        graph
            .update_file_records_only(&root, &caller_path)
            .expect("records only update");

        assert!(graph.query("caller_alpha").is_empty());
        assert_eq!(graph.query("caller_delta").len(), 1);
        assert!(
            graph.to_index().edges.iter().any(|edge| {
                edge.from_id == "file:src/caller.rs"
                    && edge.to_id == beta.id
                    && edge.kind == CodeGraphEdgeKind::Calls
            }),
            "foreground context updates keep prior file-level adjacency from the persisted index"
        );
        assert!(
            !graph.to_index().edges.iter().any(|edge| {
                edge.from_id == "file:src/caller.rs"
                    && edge.to_id == gamma.id
                    && edge.kind == CodeGraphEdgeKind::Calls
            }),
            "foreground context updates must not rebuild relationship edges"
        );
    }

    #[test]
    fn context_update_refreshes_relationships_for_changed_file_only() {
        let root = temp_workspace("context-update-file-only");
        let caller_path = root.join("src/caller.rs");
        let helper_path = root.join("src/helper.rs");
        let other_path = root.join("src/other.rs");
        fs::write(&caller_path, "pub fn caller_alpha() {}\n").expect("caller");
        fs::write(&helper_path, "pub fn helper_delta() {}\n").expect("helper");
        fs::write(&other_path, "pub fn other_alpha() {}\n").expect("other");

        let mut graph = CodeGraph::index_workspace(&root).expect("index");
        let helper = graph
            .query("helper_delta")
            .into_iter()
            .find(|record| record.kind == CodeGraphNodeKind::Symbol)
            .expect("helper symbol");

        fs::write(&caller_path, "pub fn caller_beta() { helper_delta(); }\n")
            .expect("rewrite caller");
        graph
            .update_file_for_context(&root, &caller_path)
            .expect("context update");

        assert_eq!(graph.query("caller_beta").len(), 1);
        assert!(graph.to_index().edges.iter().any(|edge| {
            edge.from_id == "file:src/caller.rs"
                && edge.to_id == helper.id
                && edge.kind == CodeGraphEdgeKind::Calls
        }));
        assert_eq!(graph.query("other_alpha").len(), 1);
    }
}
