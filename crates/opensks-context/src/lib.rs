use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use opensks_contracts::{
    CONTEXT_PACK_SCHEMA, CodeGraphEdgeKind, CodeGraphIndex, CodeGraphNodeKind, CodeGraphRecord,
    ContextPack, ContextPackBranchFreshness, ContextPackConversationSummary, ConversationDigest,
    FreshnessStamp, TriWikiRecord, TurnContextItem,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("codegraph error: {0}")]
    CodeGraph(#[from] opensks_codegraph::CodeGraphError),
    #[error("intel error: {0}")]
    Intel(#[from] opensks_intel::IntelError),
    #[error("triwiki error: {0}")]
    TriWiki(#[from] opensks_triwiki::TriWikiError),
}

pub fn build_context_pack(
    id: impl Into<String>,
    records: &[TriWikiRecord],
    token_budget: u32,
) -> ContextPack {
    let mut body = String::new();
    let mut record_ids = Vec::new();
    let mut estimated_tokens = 0u32;
    for record in records {
        let entry = format!(
            "## {}\n{}\nEvidence: {}\n\n",
            record.title,
            record.body,
            record.evidence_refs.join(", ")
        );
        let entry_tokens = estimate_tokens(&entry);
        if estimated_tokens + entry_tokens > token_budget {
            break;
        }
        estimated_tokens += entry_tokens;
        body.push_str(&entry);
        record_ids.push(record.id.clone());
    }
    ContextPack {
        schema: CONTEXT_PACK_SCHEMA.to_string(),
        id: id.into(),
        token_budget,
        estimated_tokens,
        record_ids,
        codegraph_record_ids: Vec::new(),
        changed_paths: Vec::new(),
        selected_test_targets: Vec::new(),
        turn_context_refs: Vec::new(),
        turn_context_items: Vec::new(),
        conversation_summary: None,
        branch_freshness: None,
        freshness: None,
        body,
        evidence_refs: vec!["opensks-context:triwiki-records".to_string()],
    }
}

pub fn write_context_pack(workspace: &Path, pack: &ContextPack) -> Result<PathBuf, ContextError> {
    let path = workspace
        .join(".opensks")
        .join("wiki")
        .join("context-packs")
        .join("generated")
        .join(format!("{}.json", pack.id));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(pack)? + "\n")?;
    Ok(path)
}

pub fn pack_workspace_records(
    workspace: &Path,
    id: impl Into<String>,
    token_budget: u32,
) -> Result<ContextPack, ContextError> {
    let records = opensks_triwiki::load_records(workspace)?;
    let branch_freshness = branch_freshness(workspace);
    let mut changed_paths = git_changed_paths(workspace);
    if let Some(branch) = &branch_freshness {
        changed_paths.extend(branch.changed_paths_since_base.iter().cloned());
        changed_paths.sort();
        changed_paths.dedup();
    }
    let graph = load_context_graph(workspace, &changed_paths)?;
    let selected = select_codegraph_records(&graph, &changed_paths);
    let mut freshness = opensks_intel::freshness(workspace)?;
    freshness.index_hash = graph.workspace_fingerprint.clone();
    Ok(build_workspace_context_pack(WorkspaceContextPackInput {
        id: id.into(),
        records: &records,
        codegraph_records: &selected.records,
        changed_paths,
        selected_test_targets: selected.test_targets,
        freshness: Some(freshness),
        branch_freshness,
        token_budget,
    }))
}

pub fn pack_workspace_records_with_turn_context(
    workspace: &Path,
    id: impl Into<String>,
    token_budget: u32,
    refs: &[String],
) -> Result<ContextPack, ContextError> {
    let mut pack = pack_workspace_records(workspace, id, token_budget)?;
    add_turn_context_refs(&mut pack, refs);
    add_turn_context_items(&mut pack, workspace, refs);
    Ok(pack)
}

pub fn add_conversation_summary(pack: &mut ContextPack, digest: ConversationDigest) {
    let summary = digest.summary_redacted.trim();
    if summary.is_empty() {
        return;
    }

    let secret_like = looks_secret(summary);
    let summary_redacted = if secret_like {
        "[redacted conversation summary]".to_string()
    } else {
        summary.to_string()
    };
    let reason_code = if secret_like {
        "conversation_summary_redacted_secret_like"
    } else {
        "redacted_conversation_summary"
    };
    let item = ContextPackConversationSummary {
        conversation_id: digest.conversation_id,
        summary_redacted,
        source_message_sequence: digest.source_message_sequence,
        generated_at_ms: digest.generated_at_ms,
        redacted: secret_like,
        reason_code: reason_code.to_string(),
        evidence_refs: vec![
            "opensks-conversation:conversation-summary".to_string(),
            "opensks-context:conversation-summary".to_string(),
        ],
    };

    let entry = format!(
        "## Conversation Summary\nconversation_id: {}\nsource_message_sequence: {}\ngenerated_at_ms: {}\n{}\n\n",
        item.conversation_id,
        item.source_message_sequence,
        item.generated_at_ms,
        item.summary_redacted
    );
    let _ = append_entry(
        &mut pack.body,
        &mut pack.estimated_tokens,
        pack.token_budget,
        &entry,
    );
    if !pack
        .evidence_refs
        .iter()
        .any(|reference| reference == "opensks-context:conversation-summary")
    {
        pack.evidence_refs
            .push("opensks-context:conversation-summary".to_string());
        pack.evidence_refs.sort();
    }
    pack.conversation_summary = Some(item);
}

pub fn build_worker_context_pack(
    root: &ContextPack,
    id: impl Into<String>,
    work_item_id: &str,
    role: &str,
    node_id: &str,
    token_budget: u32,
) -> ContextPack {
    let token_budget = token_budget.max(1).min(root.token_budget.max(1));
    let mut body = String::new();
    let mut estimated_tokens = 0u32;
    let mut evidence_refs = BTreeSet::from([
        "context:worker-context-pack".to_string(),
        "opensks-context:worker-scoped-context-pack".to_string(),
    ]);
    evidence_refs.extend(root.evidence_refs.iter().cloned());

    let scope = format!(
        "## Worker Scope\nwork_item_id: {work_item_id}\nrole: {role}\nnode_id: {node_id}\nroot_context_pack_id: {}\n\n",
        root.id
    );
    let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &scope);

    if let Some(stamp) = &root.freshness {
        let head = stamp.head_hash.as_deref().unwrap_or("none");
        let entry = format!(
            "## Freshness\nhead: {head}\nworktree: {}\ncodegraph_index: {}\nin_repo: {}\n\n",
            stamp.worktree_hash, stamp.index_hash, stamp.in_repo
        );
        let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
    }

    if let Some(branch) = &root.branch_freshness {
        let branch_name = branch.branch.as_deref().unwrap_or("detached");
        let base_ref = branch.base_ref.as_deref().unwrap_or("none");
        let changed = limited_bullet_list(&branch.changed_paths_since_base, 12);
        let entry = format!(
            "## Branch Freshness\nbranch: {branch_name}\nbase_ref: {base_ref}\nreason: {}\nchanged_since_base:\n{changed}\n\n",
            branch.reason_code
        );
        let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
    }

    if !root.changed_paths.is_empty() {
        let entry = format!(
            "## Changed Paths\n{}\n\n",
            limited_bullet_list(&root.changed_paths, 16)
        );
        let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
    }

    if !root.selected_test_targets.is_empty() && worker_role_needs_test_targets(role) {
        let entry = format!(
            "## Selected Test Targets\n{}\n\n",
            limited_bullet_list(&root.selected_test_targets, 12)
        );
        let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
    }

    if !root.turn_context_refs.is_empty() {
        let entry = format!(
            "## Turn Context Refs\n{}\n\n",
            limited_bullet_list(&root.turn_context_refs, 12)
        );
        let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
    }

    for item in &root.turn_context_items {
        if item.resolved && !item.stale && item.body.is_some() {
            let entry = format!(
                "## Selected Context: {}\n{}\n\n",
                item.ref_id,
                item.body.as_deref().unwrap_or_default()
            );
            let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
        }
    }

    if let Some(summary) = &root.conversation_summary {
        let entry = format!(
            "## Conversation Summary\nconversation_id: {}\nsource_message_sequence: {}\n{}\n\n",
            summary.conversation_id, summary.source_message_sequence, summary.summary_redacted
        );
        let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
    }

    if !root.codegraph_record_ids.is_empty() && worker_role_needs_codegraph(role) {
        let entry = format!(
            "## CodeGraph Record IDs\n{}\n\n",
            limited_bullet_list(&root.codegraph_record_ids, 20)
        );
        let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
    }

    if !root.record_ids.is_empty() {
        let entry = format!(
            "## TriWiki Record IDs\n{}\n\n",
            limited_bullet_list(&root.record_ids, 12)
        );
        let _ = append_entry(&mut body, &mut estimated_tokens, token_budget, &entry);
    }

    ContextPack {
        schema: CONTEXT_PACK_SCHEMA.to_string(),
        id: id.into(),
        token_budget,
        estimated_tokens,
        record_ids: root.record_ids.clone(),
        codegraph_record_ids: root.codegraph_record_ids.clone(),
        changed_paths: root.changed_paths.clone(),
        selected_test_targets: root.selected_test_targets.clone(),
        turn_context_refs: root.turn_context_refs.clone(),
        turn_context_items: root.turn_context_items.clone(),
        conversation_summary: root.conversation_summary.clone(),
        branch_freshness: root.branch_freshness.clone(),
        freshness: root.freshness.clone(),
        body,
        evidence_refs: evidence_refs.into_iter().collect(),
    }
}

fn estimate_tokens(value: &str) -> u32 {
    value.split_whitespace().count().max(1) as u32
}

fn add_turn_context_refs(pack: &mut ContextPack, refs: &[String]) {
    let mut refs = refs
        .iter()
        .filter(|reference| !reference.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    refs.sort();
    refs.dedup();
    if refs.is_empty() {
        return;
    }

    pack.turn_context_refs = refs.clone();
    if !pack
        .evidence_refs
        .iter()
        .any(|reference| reference == "opensks-context:turn-context-selection")
    {
        pack.evidence_refs
            .push("opensks-context:turn-context-selection".to_string());
        pack.evidence_refs.sort();
    }

    let entry = format!("## Turn Context Refs\n{}\n\n", bullet_list(&refs));
    let _ = append_entry(
        &mut pack.body,
        &mut pack.estimated_tokens,
        pack.token_budget,
        &entry,
    );
}

fn add_turn_context_items(pack: &mut ContextPack, workspace: &Path, refs: &[String]) {
    let items = refs
        .iter()
        .filter(|reference| !reference.trim().is_empty())
        .map(|reference| resolve_turn_context_ref(workspace, reference))
        .collect::<Vec<_>>();
    if items.is_empty() {
        return;
    }

    for item in &items {
        if item.resolved && !item.stale && item.body.is_some() {
            let entry = format!(
                "## Selected Context: {}\n{}\n\n",
                item.ref_id,
                item.body.as_deref().unwrap_or_default()
            );
            let _ = append_entry(
                &mut pack.body,
                &mut pack.estimated_tokens,
                pack.token_budget,
                &entry,
            );
        }
    }
    pack.turn_context_items = items;
}

fn resolve_turn_context_ref(workspace: &Path, reference: &str) -> TurnContextItem {
    match parse_editor_context_ref(reference) {
        Some(parsed) => resolve_editor_context_ref(workspace, reference, parsed),
        None => TurnContextItem {
            ref_id: reference.to_string(),
            kind: "opaque".to_string(),
            path: None,
            start_line: None,
            end_line: None,
            captured_hash: None,
            current_hash: None,
            resolved: false,
            stale: false,
            redacted: false,
            reason_code: "opaque_ref".to_string(),
            body: None,
            evidence_refs: vec!["opensks-context:turn-context-selection".to_string()],
        },
    }
}

#[derive(Debug, Clone)]
struct ParsedEditorContextRef {
    path: String,
    start_line: u32,
    end_line: u32,
    captured_hash: String,
}

fn parse_editor_context_ref(reference: &str) -> Option<ParsedEditorContextRef> {
    let body = reference.strip_prefix("editor://")?;
    let mut parts = body.splitn(3, '#');
    let path = parts.next()?.to_string();
    let range = parts.next()?;
    let captured_hash = parts.next()?.to_string();
    let range = range.strip_prefix('L')?;
    let (start, end) = if let Some((start, end)) = range.split_once("-L") {
        (start, end)
    } else {
        (range, range)
    };
    let start_line = start.parse::<u32>().ok()?;
    let end_line = end.parse::<u32>().ok()?;
    if path.is_empty() || start_line == 0 || end_line < start_line || captured_hash.is_empty() {
        return None;
    }
    Some(ParsedEditorContextRef {
        path,
        start_line,
        end_line,
        captured_hash,
    })
}

fn resolve_editor_context_ref(
    workspace: &Path,
    reference: &str,
    parsed: ParsedEditorContextRef,
) -> TurnContextItem {
    let evidence_refs = vec![
        "opensks-context:turn-context-selection".to_string(),
        "opensks-context:editor-range-resolver".to_string(),
    ];
    let Some(path) = safe_workspace_file(workspace, &parsed.path) else {
        return editor_context_item(
            reference,
            parsed,
            EditorContextItemStatus {
                current_hash: None,
                stale: false,
                redacted: false,
                reason_code: "path_escape_or_absolute",
                body: None,
                evidence_refs,
            },
        );
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => {
            return editor_context_item(
                reference,
                parsed,
                EditorContextItemStatus {
                    current_hash: None,
                    stale: false,
                    redacted: false,
                    reason_code: "unreadable_file",
                    body: None,
                    evidence_refs,
                },
            );
        }
    };
    let Some(selection) = selected_lines(&text, parsed.start_line, parsed.end_line) else {
        return editor_context_item(
            reference,
            parsed,
            EditorContextItemStatus {
                current_hash: None,
                stale: false,
                redacted: true,
                reason_code: "line_range_unavailable",
                body: None,
                evidence_refs,
            },
        );
    };
    let current_hash = fnv1a64(selection.as_bytes());
    if current_hash != parsed.captured_hash {
        return editor_context_item(
            reference,
            parsed,
            EditorContextItemStatus {
                current_hash: Some(current_hash),
                stale: true,
                redacted: false,
                reason_code: "selection_hash_mismatch",
                body: None,
                evidence_refs,
            },
        );
    }
    if looks_secret(&selection) {
        return editor_context_item(
            reference,
            parsed,
            EditorContextItemStatus {
                current_hash: Some(current_hash),
                stale: false,
                redacted: true,
                reason_code: "selected_context_redacted_secret_like",
                body: Some("[redacted selected context]".to_string()),
                evidence_refs,
            },
        );
    }
    editor_context_item(
        reference,
        parsed,
        EditorContextItemStatus {
            current_hash: Some(current_hash),
            stale: false,
            redacted: false,
            reason_code: "fresh",
            body: Some(selection),
            evidence_refs,
        },
    )
}

#[derive(Debug)]
struct EditorContextItemStatus {
    current_hash: Option<String>,
    stale: bool,
    redacted: bool,
    reason_code: &'static str,
    body: Option<String>,
    evidence_refs: Vec<String>,
}

fn editor_context_item(
    reference: &str,
    parsed: ParsedEditorContextRef,
    status: EditorContextItemStatus,
) -> TurnContextItem {
    TurnContextItem {
        ref_id: reference.to_string(),
        kind: "editor_range".to_string(),
        path: Some(parsed.path),
        start_line: Some(parsed.start_line),
        end_line: Some(parsed.end_line),
        captured_hash: Some(parsed.captured_hash),
        current_hash: status.current_hash,
        resolved: status.reason_code == "fresh"
            || status.reason_code == "selected_context_redacted_secret_like",
        stale: status.stale,
        redacted: status.redacted,
        reason_code: status.reason_code.to_string(),
        body: status.body,
        evidence_refs: status.evidence_refs,
    }
}

struct WorkspaceContextPackInput<'a> {
    id: String,
    records: &'a [TriWikiRecord],
    codegraph_records: &'a [CodeGraphRecord],
    changed_paths: Vec<String>,
    selected_test_targets: Vec<String>,
    freshness: Option<FreshnessStamp>,
    branch_freshness: Option<ContextPackBranchFreshness>,
    token_budget: u32,
}

fn build_workspace_context_pack(input: WorkspaceContextPackInput<'_>) -> ContextPack {
    let WorkspaceContextPackInput {
        id,
        records,
        codegraph_records,
        changed_paths,
        selected_test_targets,
        freshness,
        branch_freshness,
        token_budget,
    } = input;
    let mut body = String::new();
    let mut record_ids = Vec::new();
    let mut codegraph_record_ids = Vec::new();
    let mut estimated_tokens = 0u32;
    let mut evidence_refs = BTreeSet::from(["opensks-context:triwiki-records".to_string()]);

    if let Some(stamp) = &freshness {
        let head = stamp.head_hash.as_deref().unwrap_or("none");
        let entry = format!(
            "## Freshness\nhead: {head}\nworktree: {}\ncodegraph_index: {}\nin_repo: {}\n\n",
            stamp.worktree_hash, stamp.index_hash, stamp.in_repo
        );
        if append_entry(&mut body, &mut estimated_tokens, token_budget, &entry) {
            evidence_refs.insert("opensks-intel:freshness".to_string());
        }
    }

    if let Some(branch) = &branch_freshness {
        let branch_name = branch.branch.as_deref().unwrap_or("detached");
        let head = branch.head_hash.as_deref().unwrap_or("none");
        let base_ref = branch.base_ref.as_deref().unwrap_or("none");
        let merge_base = branch.merge_base.as_deref().unwrap_or("none");
        let ahead = branch
            .ahead_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let behind = branch
            .behind_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let changed = if branch.changed_paths_since_base.is_empty() {
            "none".to_string()
        } else {
            bullet_list(&branch.changed_paths_since_base)
        };
        let entry = format!(
            "## Branch Freshness\nbranch: {branch_name}\nhead: {head}\nbase_ref: {base_ref}\nmerge_base: {merge_base}\nahead: {ahead}\nbehind: {behind}\nreason: {}\nchanged_since_base:\n{changed}\n\n",
            branch.reason_code
        );
        if append_entry(&mut body, &mut estimated_tokens, token_budget, &entry) {
            evidence_refs.insert("opensks-context:branch-freshness".to_string());
        }
    }

    if !changed_paths.is_empty() {
        let entry = format!("## Changed Paths\n{}\n\n", bullet_list(&changed_paths));
        if append_entry(&mut body, &mut estimated_tokens, token_budget, &entry) {
            evidence_refs.insert("opensks-context:changed-paths".to_string());
        }
    }

    if !selected_test_targets.is_empty() {
        let entry = format!(
            "## Selected Test Targets\n{}\n\n",
            bullet_list(&selected_test_targets)
        );
        if append_entry(&mut body, &mut estimated_tokens, token_budget, &entry) {
            evidence_refs.insert("opensks-context:test-selection".to_string());
        }
    }

    for record in records {
        let entry = format!(
            "## TriWiki: {}\n{}\nEvidence: {}\n\n",
            record.title,
            record.body,
            record.evidence_refs.join(", ")
        );
        if append_entry(&mut body, &mut estimated_tokens, token_budget, &entry) {
            record_ids.push(record.id.clone());
        }
    }

    for record in codegraph_records {
        let entry = format!(
            "## CodeGraph: {}\nkind: {}\npath: {}:{}\ncontent_hash: {}\nEvidence: {}\n\n",
            record.name,
            kind_label(&record.kind),
            record.path,
            record.line,
            record.content_hash,
            record.evidence_refs.join(", ")
        );
        if append_entry(&mut body, &mut estimated_tokens, token_budget, &entry) {
            codegraph_record_ids.push(record.id.clone());
            evidence_refs.insert("opensks-context:codegraph-records".to_string());
            if record
                .evidence_refs
                .iter()
                .any(|reference| reference == "opensks-context:codegraph-adjacent")
            {
                evidence_refs.insert("opensks-context:codegraph-adjacent".to_string());
            }
        }
    }

    ContextPack {
        schema: CONTEXT_PACK_SCHEMA.to_string(),
        id,
        token_budget,
        estimated_tokens,
        record_ids,
        codegraph_record_ids,
        changed_paths,
        selected_test_targets,
        turn_context_refs: Vec::new(),
        turn_context_items: Vec::new(),
        conversation_summary: None,
        branch_freshness,
        freshness,
        body,
        evidence_refs: evidence_refs.into_iter().collect(),
    }
}

fn append_entry(
    body: &mut String,
    estimated_tokens: &mut u32,
    token_budget: u32,
    entry: &str,
) -> bool {
    let entry_tokens = estimate_tokens(entry);
    if *estimated_tokens + entry_tokens > token_budget {
        return false;
    }
    *estimated_tokens += entry_tokens;
    body.push_str(entry);
    true
}

fn load_context_graph(
    workspace: &Path,
    changed_paths: &[String],
) -> Result<CodeGraphIndex, ContextError> {
    let mut graph = match opensks_codegraph::read_index(workspace)? {
        Some(graph) => graph,
        None => opensks_codegraph::CodeGraph::index_workspace(workspace)?,
    };

    for path in changed_paths {
        if !is_supported_source_path(path) {
            continue;
        }
        let absolute = workspace.join(path);
        if absolute.is_file() {
            graph.update_file(workspace, &absolute)?;
        }
    }

    Ok(graph.to_index())
}

#[derive(Debug, Default)]
struct SelectedCodeGraphRecords {
    records: Vec<CodeGraphRecord>,
    test_targets: Vec<String>,
}

fn select_codegraph_records(
    graph: &CodeGraphIndex,
    changed_paths: &[String],
) -> SelectedCodeGraphRecords {
    let changed: BTreeSet<_> = changed_paths.iter().cloned().collect();
    let mut selected = BTreeMap::new();
    let mut terms = BTreeSet::new();

    for path in changed_paths {
        if let Some(stem) = Path::new(path).file_stem().and_then(|stem| stem.to_str()) {
            terms.insert(stem.to_ascii_lowercase());
        }
    }

    for record in &graph.records {
        if changed.contains(&record.path) {
            if record.kind != CodeGraphNodeKind::File {
                terms.insert(record.name.to_ascii_lowercase());
            }
            selected.insert(record.id.clone(), record.clone());
        }
    }

    let mut test_targets = BTreeSet::new();
    for record in graph
        .records
        .iter()
        .filter(|record| record.kind == CodeGraphNodeKind::Test)
    {
        let name = record.name.to_ascii_lowercase();
        let path = record.path.to_ascii_lowercase();
        let related = changed.contains(&record.path)
            || terms
                .iter()
                .any(|term| !term.is_empty() && (name.contains(term) || path.contains(term)));
        if related {
            selected.insert(record.id.clone(), record.clone());
            test_targets.insert(test_target(record));
        }
    }

    add_adjacent_codegraph_records(graph, &mut selected);

    if selected.is_empty() {
        for record in graph
            .records
            .iter()
            .filter(|record| record.kind != CodeGraphNodeKind::File)
            .take(16)
        {
            selected.insert(record.id.clone(), record.clone());
        }
    }

    let mut records: Vec<_> = selected.into_values().collect();
    records.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
    });

    SelectedCodeGraphRecords {
        records,
        test_targets: test_targets.into_iter().collect(),
    }
}

fn add_adjacent_codegraph_records(
    graph: &CodeGraphIndex,
    selected: &mut BTreeMap<String, CodeGraphRecord>,
) {
    let selected_ids = selected.keys().cloned().collect::<BTreeSet<_>>();
    if selected_ids.is_empty() {
        return;
    }
    let records_by_id = graph
        .records
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    let mut adjacent = BTreeMap::new();
    for edge in &graph.edges {
        let neighbor_id = if selected_ids.contains(&edge.from_id) {
            Some(edge.to_id.as_str())
        } else if selected_ids.contains(&edge.to_id) {
            Some(edge.from_id.as_str())
        } else {
            None
        };
        let Some(neighbor_id) = neighbor_id else {
            continue;
        };
        let Some(record) = records_by_id.get(neighbor_id) else {
            continue;
        };
        let mut record = (*record).clone();
        push_unique_evidence(&mut record, "opensks-context:codegraph-adjacent");
        push_unique_evidence(
            &mut record,
            &format!(
                "opensks-context:codegraph-edge:{}",
                edge_kind_label(&edge.kind)
            ),
        );
        adjacent.insert(record.id.clone(), record);
    }
    for record in adjacent.into_values() {
        if let Some(existing) = selected.get_mut(&record.id) {
            for evidence_ref in record.evidence_refs {
                push_unique_evidence(existing, &evidence_ref);
            }
        } else {
            selected.insert(record.id.clone(), record);
        }
    }
}

fn push_unique_evidence(record: &mut CodeGraphRecord, evidence_ref: &str) {
    if !record
        .evidence_refs
        .iter()
        .any(|existing| existing == evidence_ref)
    {
        record.evidence_refs.push(evidence_ref.to_string());
        record.evidence_refs.sort();
    }
}

fn test_target(record: &CodeGraphRecord) -> String {
    format!("{}:{}:{}", record.path, record.line, record.name)
}

fn bullet_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("- {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn limited_bullet_list(values: &[String], limit: usize) -> String {
    let mut shown = values.iter().take(limit).cloned().collect::<Vec<_>>();
    if values.len() > limit {
        shown.push(format!("... {} more", values.len() - limit));
    }
    bullet_list(&shown)
}

fn worker_role_needs_test_targets(role: &str) -> bool {
    matches!(
        role,
        "code" | "verifier" | "verification" | "planning" | "planner" | "general"
    )
}

fn worker_role_needs_codegraph(role: &str) -> bool {
    !matches!(role, "image" | "vision")
}

fn safe_workspace_file(workspace: &Path, relative: &str) -> Option<PathBuf> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    let path = workspace.join(relative_path);
    if path.is_file() { Some(path) } else { None }
}

fn selected_lines(text: &str, start_line: u32, end_line: u32) -> Option<String> {
    let start = usize::try_from(start_line).ok()?.checked_sub(1)?;
    let end = usize::try_from(end_line).ok()?.checked_sub(1)?;
    let lines = text.lines().collect::<Vec<_>>();
    if start > end || end >= lines.len() {
        return None;
    }
    Some(lines[start..=end].join("\n"))
}

fn fnv1a64(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn looks_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("api_key")
        || lower.contains("secret=")
        || lower.contains("bearer ")
        || lower.contains("sk-")
        || lower.contains("password=")
}

fn git_changed_paths(workspace: &Path) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain", "-z", "--"])
        .current_dir(workspace)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut paths = Vec::new();
    for field in stdout.split('\0') {
        if field.is_empty() {
            continue;
        }
        if field.len() >= 3 && field.as_bytes()[2] == b' ' {
            paths.push(field[3..].replace('\\', "/"));
        } else {
            paths.push(field.replace('\\', "/"));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn branch_freshness(workspace: &Path) -> Option<ContextPackBranchFreshness> {
    if git_text(workspace, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return None;
    }
    let branch = git_text(workspace, &["branch", "--show-current"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let head_hash = git_text(workspace, &["rev-parse", "HEAD"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let upstream = git_text(
        workspace,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());
    let fallback_base = upstream.clone().or_else(|| {
        first_existing_ref(
            workspace,
            &["origin/main", "origin/master", "main", "master"],
        )
    });
    let Some(base_ref) = fallback_base else {
        return Some(ContextPackBranchFreshness {
            branch,
            head_hash,
            base_ref: None,
            merge_base: None,
            changed_paths_since_base: Vec::new(),
            ahead_count: None,
            behind_count: None,
            reason_code: "no_branch_base_ref".to_string(),
            evidence_refs: vec!["opensks-context:branch-freshness".to_string()],
        });
    };
    let merge_base = git_text(workspace, &["merge-base", "HEAD", &base_ref])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(merge_base_value) = merge_base.clone() else {
        return Some(ContextPackBranchFreshness {
            branch,
            head_hash,
            base_ref: Some(base_ref),
            merge_base: None,
            changed_paths_since_base: Vec::new(),
            ahead_count: None,
            behind_count: None,
            reason_code: "no_branch_merge_base".to_string(),
            evidence_refs: vec!["opensks-context:branch-freshness".to_string()],
        });
    };
    let changed_paths_since_base = git_name_only(workspace, &merge_base_value, "HEAD");
    let (ahead_count, behind_count) = ahead_behind_counts(workspace, &base_ref);
    Some(ContextPackBranchFreshness {
        branch,
        head_hash,
        base_ref: Some(base_ref),
        merge_base,
        changed_paths_since_base,
        ahead_count,
        behind_count,
        reason_code: "branch_compared_to_base".to_string(),
        evidence_refs: vec!["opensks-context:branch-freshness".to_string()],
    })
}

fn first_existing_ref(workspace: &Path, refs: &[&str]) -> Option<String> {
    refs.iter()
        .find(|reference| git_success(workspace, &["rev-parse", "--verify", reference]))
        .map(|reference| (*reference).to_string())
}

fn git_name_only(workspace: &Path, from: &str, to: &str) -> Vec<String> {
    let range = format!("{from}..{to}");
    let Some(output) = git_text(workspace, &["diff", "--name-only", "-z", &range, "--"]) else {
        return Vec::new();
    };
    let mut paths = output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(|path| path.replace('\\', "/"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn ahead_behind_counts(workspace: &Path, base_ref: &str) -> (Option<u32>, Option<u32>) {
    let range = format!("{base_ref}...HEAD");
    let Some(output) = git_text(workspace, &["rev-list", "--left-right", "--count", &range]) else {
        return (None, None);
    };
    let mut parts = output.split_whitespace();
    let behind = parts.next().and_then(|value| value.parse::<u32>().ok());
    let ahead = parts.next().and_then(|value| value.parse::<u32>().ok());
    (ahead, behind)
}

fn git_success(workspace: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn git_text(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).to_string())
}

fn is_supported_source_path(path: &str) -> bool {
    matches!(
        Path::new(path).extension().and_then(|ext| ext.to_str()),
        Some("rs" | "swift" | "ts" | "tsx" | "js")
    )
}

fn kind_label(kind: &CodeGraphNodeKind) -> &'static str {
    match kind {
        CodeGraphNodeKind::File => "file",
        CodeGraphNodeKind::Symbol => "symbol",
        CodeGraphNodeKind::Import => "import",
        CodeGraphNodeKind::Test => "test",
        CodeGraphNodeKind::Route => "route",
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use opensks_contracts::TriWikiRecordKind;
    use std::process::Command;

    fn temp_workspace(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "opensks-context-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).expect("workspace");
        root
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn context_pack_respects_token_budget() {
        let records = vec![
            opensks_triwiki::make_record(
                "a",
                TriWikiRecordKind::Claim,
                "Short",
                "one two",
                Vec::new(),
                Vec::new(),
            )
            .expect("record"),
            opensks_triwiki::make_record(
                "b",
                TriWikiRecordKind::Claim,
                "Long",
                "three four five six seven eight nine",
                Vec::new(),
                Vec::new(),
            )
            .expect("record"),
        ];
        let pack = build_context_pack("pack", &records, 8);
        assert_eq!(pack.record_ids, vec!["a"]);
        assert!(pack.estimated_tokens <= 8);
    }

    #[test]
    fn workspace_pack_includes_codegraph_freshness_and_changed_tests() {
        let root = temp_workspace("workspace");
        run_git(&root, &["init"]);
        run_git(&root, &["config", "user.email", "context@example.test"]);
        run_git(&root, &["config", "user.name", "Context Test"]);

        let source = root.join("src/lib.rs");
        fs::write(
            root.join("src/helper.rs"),
            "pub fn helper_dependency() {}\n",
        )
        .expect("seed helper");
        fs::write(&source, "pub fn alpha() {}\n#[test]\nfn alpha_test() {}\n")
            .expect("seed source");
        let record = opensks_triwiki::make_record(
            "architecture-context",
            TriWikiRecordKind::Architecture,
            "Context",
            "Context packs combine architecture notes with code graph evidence.",
            vec!["architecture".to_string()],
            vec!["docs/runtime-truth-matrix.md".to_string()],
        )
        .expect("record");
        opensks_triwiki::append_record(&root, &record).expect("append record");
        let graph = opensks_codegraph::CodeGraph::index_workspace(&root).expect("index");
        opensks_codegraph::write_index(&root, &graph).expect("write index");
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-m", "seed context"]);
        run_git(&root, &["branch", "-M", "main"]);
        run_git(&root, &["checkout", "-b", "feature/context-pack"]);

        fs::write(
            &source,
            "use crate::helper::helper_dependency;\npub fn alpha_changed() { helper_dependency(); }\n#[test]\nfn alpha_changed_test() { alpha_changed(); }\n",
        )
        .expect("edit source");
        run_git(&root, &["add", "src/lib.rs"]);
        run_git(&root, &["commit", "-m", "feature context change"]);

        let selected_hash = fnv1a64("pub fn alpha_changed() { helper_dependency(); }".as_bytes());
        let pack = pack_workspace_records_with_turn_context(
            &root,
            "worker-context",
            300,
            &[
                format!("editor://src/lib.rs#L2-L2#{selected_hash}"),
                "symbol://alpha_changed".to_string(),
            ],
        )
        .expect("pack");
        assert_eq!(pack.changed_paths, vec!["src/lib.rs"]);
        assert_eq!(
            pack.turn_context_refs,
            vec![
                format!("editor://src/lib.rs#L2-L2#{selected_hash}"),
                "symbol://alpha_changed".to_string()
            ]
        );
        let editor_item = pack
            .turn_context_items
            .iter()
            .find(|item| item.kind == "editor_range")
            .expect("editor context item");
        assert!(editor_item.resolved);
        assert!(!editor_item.stale);
        assert_eq!(editor_item.reason_code, "fresh");
        assert_eq!(
            editor_item.body.as_deref(),
            Some("pub fn alpha_changed() { helper_dependency(); }")
        );
        assert!(
            pack.turn_context_items
                .iter()
                .any(|item| item.kind == "opaque" && item.ref_id == "symbol://alpha_changed")
        );
        assert!(pack.freshness.is_some());
        let branch = pack.branch_freshness.as_ref().expect("branch freshness");
        assert_eq!(branch.branch.as_deref(), Some("feature/context-pack"));
        assert_eq!(branch.base_ref.as_deref(), Some("main"));
        assert_eq!(branch.reason_code, "branch_compared_to_base");
        assert_eq!(branch.ahead_count, Some(1));
        assert_eq!(branch.behind_count, Some(0));
        assert_eq!(branch.changed_paths_since_base, vec!["src/lib.rs"]);
        assert!(
            pack.codegraph_record_ids
                .iter()
                .any(|id| id.contains("alpha_changed")),
            "changed symbols should be refreshed into the worker pack"
        );
        assert!(
            pack.codegraph_record_ids
                .iter()
                .any(|id| id.contains("helper_dependency")),
            "generated call edges should bring adjacent helper symbols into the worker pack"
        );
        assert!(
            pack.selected_test_targets
                .iter()
                .any(|target| target.contains("alpha_changed_test")),
            "changed-symbol related test target should be selected"
        );
        assert!(pack.body.contains("## Freshness"));
        assert!(pack.body.contains("## Branch Freshness"));
        assert!(pack.body.contains("base_ref: main"));
        assert!(pack.body.contains("## Turn Context Refs"));
        assert!(pack.body.contains("## Changed Paths"));
        assert!(pack.body.contains("## CodeGraph: alpha_changed"));
        assert!(pack.body.contains("## CodeGraph: helper_dependency"));
        assert!(
            pack.evidence_refs
                .iter()
                .any(|reference| reference == "opensks-context:codegraph-records")
        );
        assert!(
            pack.evidence_refs
                .iter()
                .any(|reference| reference == "opensks-context:codegraph-adjacent")
        );

        let worker_pack = build_worker_context_pack(
            &pack,
            "worker-context-code",
            "turn-role-1-code",
            "code",
            "conversation-turn-role-code",
            120,
        );
        assert_eq!(worker_pack.id, "worker-context-code");
        assert!(worker_pack.estimated_tokens <= worker_pack.token_budget);
        assert!(worker_pack.body.len() < pack.body.len());
        assert!(worker_pack.body.contains("## Worker Scope"));
        assert!(worker_pack.body.contains("work_item_id: turn-role-1-code"));
        assert!(worker_pack.body.contains("## Selected Context"));
        assert!(worker_pack.body.contains("alpha_changed"));
        assert!(worker_pack.body.contains("## CodeGraph Record IDs"));
        assert!(!worker_pack.body.contains("## TriWiki:"));
        assert!(
            worker_pack
                .evidence_refs
                .iter()
                .any(|reference| reference == "opensks-context:worker-scoped-context-pack")
        );
    }

    #[test]
    fn codegraph_selection_includes_adjacent_reference_records() {
        let changed = CodeGraphRecord {
            schema: "opensks.codegraph-record.v1".to_string(),
            id: "src/lib.rs:1:alpha_changed".to_string(),
            kind: CodeGraphNodeKind::Symbol,
            path: "src/lib.rs".to_string(),
            name: "alpha_changed".to_string(),
            line: 1,
            content_hash: "fnv1a64:changed".to_string(),
            evidence_refs: vec!["fixture:changed".to_string()],
        };
        let helper = CodeGraphRecord {
            schema: "opensks.codegraph-record.v1".to_string(),
            id: "src/helper.rs:1:helper_dependency".to_string(),
            kind: CodeGraphNodeKind::Symbol,
            path: "src/helper.rs".to_string(),
            name: "helper_dependency".to_string(),
            line: 1,
            content_hash: "fnv1a64:helper".to_string(),
            evidence_refs: vec!["fixture:helper".to_string()],
        };
        let graph = CodeGraphIndex {
            schema: "opensks.codegraph-index.v1".to_string(),
            workspace_fingerprint: "fnv1a64:index".to_string(),
            records: vec![
                CodeGraphRecord {
                    schema: "opensks.codegraph-record.v1".to_string(),
                    id: "file:src/lib.rs".to_string(),
                    kind: CodeGraphNodeKind::File,
                    path: "src/lib.rs".to_string(),
                    name: "lib.rs".to_string(),
                    line: 0,
                    content_hash: "fnv1a64:changed".to_string(),
                    evidence_refs: vec!["fixture:file".to_string()],
                },
                changed.clone(),
                helper.clone(),
            ],
            edges: vec![opensks_contracts::CodeGraphEdge {
                from_id: changed.id.clone(),
                to_id: helper.id.clone(),
                kind: CodeGraphEdgeKind::References,
            }],
            freshness: "fresh".to_string(),
        };

        let selected = select_codegraph_records(&graph, &["src/lib.rs".to_string()]);
        let helper_record = selected
            .records
            .iter()
            .find(|record| record.id == helper.id)
            .expect("adjacent helper record");
        assert!(
            helper_record
                .evidence_refs
                .iter()
                .any(|reference| reference == "opensks-context:codegraph-adjacent")
        );
        assert!(
            helper_record
                .evidence_refs
                .iter()
                .any(|reference| reference == "opensks-context:codegraph-edge:references")
        );

        let pack = build_workspace_context_pack(WorkspaceContextPackInput {
            id: "adjacent-context".to_string(),
            records: &[],
            codegraph_records: &selected.records,
            changed_paths: vec!["src/lib.rs".to_string()],
            selected_test_targets: Vec::new(),
            freshness: None,
            branch_freshness: None,
            token_budget: 200,
        });
        assert!(
            pack.codegraph_record_ids
                .iter()
                .any(|id| id == "src/helper.rs:1:helper_dependency")
        );
        assert!(pack.body.contains("## CodeGraph: helper_dependency"));
        assert!(
            pack.evidence_refs
                .iter()
                .any(|reference| reference == "opensks-context:codegraph-adjacent")
        );
    }

    #[test]
    fn selected_context_items_fail_closed_for_stale_and_secret_like_ranges() {
        let root = temp_workspace("selected-context-safety");
        fs::write(root.join("src/lib.rs"), "pub fn current() {}\n").expect("source");
        fs::write(root.join("src/secret.rs"), "let api_key = \"sk-test\";\n").expect("secret");
        let secret_hash = fnv1a64("let api_key = \"sk-test\";".as_bytes());

        let pack = pack_workspace_records_with_turn_context(
            &root,
            "selected-context-safety",
            320,
            &[
                "editor://src/lib.rs#L1-L1#fnv1a64:stale".to_string(),
                format!("editor://src/secret.rs#L1-L1#{secret_hash}"),
            ],
        )
        .expect("pack");

        let stale = pack
            .turn_context_items
            .iter()
            .find(|item| item.path.as_deref() == Some("src/lib.rs"))
            .expect("stale item");
        assert!(stale.stale);
        assert_eq!(stale.reason_code, "selection_hash_mismatch");
        assert!(stale.body.is_none());

        let redacted = pack
            .turn_context_items
            .iter()
            .find(|item| item.path.as_deref() == Some("src/secret.rs"))
            .expect("redacted item");
        assert!(redacted.resolved);
        assert!(redacted.redacted);
        assert_eq!(
            redacted.reason_code,
            "selected_context_redacted_secret_like"
        );
        assert_eq!(
            redacted.body.as_deref(),
            Some("[redacted selected context]")
        );
        assert!(!pack.body.contains("sk-test"));
        assert!(
            !serde_json::to_string(&pack)
                .expect("pack json")
                .contains("sk-test")
        );
    }

    #[test]
    fn conversation_summary_is_structured_budgeted_and_defensively_redacted() {
        let mut safe = build_context_pack("pack-safe-summary", &[], 80);
        add_conversation_summary(
            &mut safe,
            ConversationDigest {
                schema: "opensks.conversation-digest.v1".to_string(),
                conversation_id: "conversation-safe".to_string(),
                summary_redacted: "User asked to wire context packs into worker planning."
                    .to_string(),
                source_message_sequence: 7,
                generated_at_ms: 1_700,
            },
        );
        let summary = safe.conversation_summary.expect("safe summary");
        assert_eq!(summary.conversation_id, "conversation-safe");
        assert_eq!(summary.source_message_sequence, 7);
        assert_eq!(summary.generated_at_ms, 1_700);
        assert!(!summary.redacted);
        assert_eq!(summary.reason_code, "redacted_conversation_summary");
        assert!(safe.body.contains("## Conversation Summary"));
        assert!(safe.body.contains("worker planning"));
        assert!(
            safe.evidence_refs
                .iter()
                .any(|reference| reference == "opensks-context:conversation-summary")
        );

        let mut secret_like = build_context_pack("pack-secret-summary", &[], 80);
        add_conversation_summary(
            &mut secret_like,
            ConversationDigest {
                schema: "opensks.conversation-digest.v1".to_string(),
                conversation_id: "conversation-secret".to_string(),
                summary_redacted: "User pasted password=sk-test".to_string(),
                source_message_sequence: 8,
                generated_at_ms: 1_800,
            },
        );
        let summary = secret_like
            .conversation_summary
            .as_ref()
            .expect("secret-like summary");
        assert!(summary.redacted);
        assert_eq!(
            summary.reason_code,
            "conversation_summary_redacted_secret_like"
        );
        assert_eq!(summary.summary_redacted, "[redacted conversation summary]");
        assert!(!secret_like.body.contains("sk-test"));
        assert!(
            !serde_json::to_string(&secret_like)
                .expect("pack json")
                .contains("sk-test")
        );
    }
}
