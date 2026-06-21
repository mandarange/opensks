use std::fs;
use std::path::{Path, PathBuf};

use opensks_contracts::{CONTEXT_PACK_SCHEMA, ContextPack, TriWikiRecord};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ContextError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
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
    Ok(build_context_pack(id, &records, token_budget))
}

fn estimate_tokens(value: &str) -> u32 {
    value.split_whitespace().count().max(1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensks_contracts::TriWikiRecordKind;

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
}
