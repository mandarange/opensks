use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use opensks_contracts::{TRIWIKI_RECORD_SCHEMA, TriWikiRecord, TriWikiRecordKind};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TriWikiError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("secret-looking content cannot be written to shared TriWiki records")]
    SecretPublishBlocked,
    #[error("record shard does not match derived shard for id")]
    ShardMismatch,
}

pub fn make_record(
    id: impl Into<String>,
    kind: TriWikiRecordKind,
    title: impl Into<String>,
    body: impl Into<String>,
    tags: Vec<String>,
    evidence_refs: Vec<String>,
) -> Result<TriWikiRecord, TriWikiError> {
    let title = title.into();
    let body = body.into();
    if looks_secret(&title) || looks_secret(&body) {
        return Err(TriWikiError::SecretPublishBlocked);
    }
    let id = id.into();
    Ok(TriWikiRecord {
        schema: TRIWIKI_RECORD_SCHEMA.to_string(),
        shard: shard_for_id(&id),
        id,
        kind,
        title,
        body,
        tags,
        evidence_refs,
        redacted: true,
    })
}

pub fn append_record(workspace: &Path, record: &TriWikiRecord) -> Result<PathBuf, TriWikiError> {
    if looks_secret(&record.title) || looks_secret(&record.body) {
        return Err(TriWikiError::SecretPublishBlocked);
    }
    let expected_shard = shard_for_id(&record.id);
    if record.shard != expected_shard {
        return Err(TriWikiError::ShardMismatch);
    }
    let path = workspace
        .join(".opensks")
        .join("wiki")
        .join("records")
        .join(&expected_shard);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("jsonl.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    lock_file.lock_exclusive()?;
    if path.exists() {
        for line in fs::read_to_string(&path)?.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let existing: TriWikiRecord = serde_json::from_str(line)?;
            if existing.id == record.id {
                return Ok(path);
            }
        }
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(serde_json::to_string(record)?.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(path)
}

pub fn load_records(workspace: &Path) -> Result<Vec<TriWikiRecord>, TriWikiError> {
    let dir = workspace.join(".opensks").join("wiki").join("records");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        for line in fs::read_to_string(path)?.lines() {
            if line.trim().is_empty() {
                continue;
            }
            records.push(serde_json::from_str(line)?);
        }
    }
    records.sort_by(|left: &TriWikiRecord, right| left.id.cmp(&right.id));
    Ok(records)
}

pub fn shard_for_id(id: &str) -> String {
    let prefix: String = id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(2)
        .collect();
    format!(
        "{}.jsonl",
        if prefix.is_empty() {
            "zz".to_string()
        } else {
            prefix.to_ascii_lowercase()
        }
    )
}

fn looks_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("api_key")
        || lower.contains("secret=")
        || lower.contains("bearer ")
        || lower.contains("sk-")
        || lower.contains("password=")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("opensks-triwiki-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("workspace");
        root
    }

    #[test]
    fn append_records_uses_merge_friendly_shards() {
        let root = temp_workspace("append");
        let a = make_record(
            "architecture-engine",
            TriWikiRecordKind::Architecture,
            "Engine",
            "Event store is source of truth.",
            vec!["architecture".to_string()],
            vec!["docs/runtime-truth-matrix.md".to_string()],
        )
        .expect("record");
        let b = make_record(
            "glossary-work-item",
            TriWikiRecordKind::Glossary,
            "WorkItem",
            "Schedulable unit.",
            vec!["glossary".to_string()],
            Vec::new(),
        )
        .expect("record");
        append_record(&root, &a).expect("append a");
        append_record(&root, &b).expect("append b");
        let records = load_records(&root).expect("load");
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .any(|record| record.kind == TriWikiRecordKind::Glossary)
        );
    }

    #[test]
    fn shared_record_write_blocks_secret_content() {
        let error = make_record(
            "bad",
            TriWikiRecordKind::Wrongness,
            "Leaked",
            "api_key=sk-test",
            Vec::new(),
            Vec::new(),
        )
        .expect_err("secret block");
        assert!(matches!(error, TriWikiError::SecretPublishBlocked));
    }
}
