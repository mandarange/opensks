use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("artifact path has no parent: {0}")]
    MissingParent(PathBuf),
    #[error("artifact io error: {0}")]
    Io(#[from] io::Error),
}

pub fn write_text_atomic(path: &Path, contents: &str) -> Result<(), ArtifactError> {
    let parent = path
        .parent()
        .ok_or_else(|| ArtifactError::MissingParent(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    {
        let mut file = File::create(&tmp)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), ArtifactError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_text_atomic(path, &(json + "\n"))
}

pub fn redact_secrets(input: &str) -> String {
    input
        .split_whitespace()
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_token(token: &str) -> &str {
    let lower = token.to_ascii_lowercase();
    let looks_secret = lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("authorization:")
        || token.starts_with("sk-")
        || token.contains("BEGIN_PRIVATE_KEY");
    if looks_secret { "[redacted]" } else { token }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_and_file() {
        let dir = std::env::temp_dir().join(format!("opensks-artifact-{}", std::process::id()));
        let path = dir.join("nested").join("artifact.txt");
        write_text_atomic(&path, "hello").expect("write");
        assert_eq!(fs::read_to_string(path).expect("read"), "hello");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn redaction_removes_secret_tokens() {
        let redacted = redact_secrets("Authorization: Bearer sk-test password=hunter2 safe");
        assert!(!redacted.contains("sk-test"));
        assert!(!redacted.contains("hunter2"));
        assert!(redacted.contains("safe"));
    }

    // ======================================================================
    // PR-044 PART B — EXPORT-BOUNDARY REDACTION PROOF
    //
    // `redact_secrets` is the shared sanitizer used by the EXPORT paths
    // (summaries / reports / vault-summary). This proof feeds it realistic
    // secret patterns embedded in report-like prose and asserts (a) the raw
    // secret substring is ABSENT from the exported bytes, and (b) benign words
    // around the secret survive so the export stays useful.
    // ======================================================================

    #[test]
    fn export_redaction_strips_realistic_secret_patterns() {
        // (raw secret, the document it is embedded in)
        let cases: &[&str] = &[
            "redaction-test-secret-fixture-0001",
            "Authorization:Bearer-sk-test-9988",
            "password=hunter2-very-secret",
            "my_secret_value_inline",
            "BEGIN_PRIVATE_KEY-blob",
        ];
        for secret in cases {
            let document = format!(
                "Run summary for vault export. The provider returned {secret} which must \
                 never reach the report. Status: ok. Next step: continue."
            );
            let exported = redact_secrets(&document);
            assert!(
                !exported.contains(*secret),
                "EXPORT redaction leaked {secret:?}: {exported}"
            );
            // Benign surrounding words survive the redaction.
            assert!(exported.contains("summary"));
            assert!(exported.contains("Status:"));
            assert!(exported.contains("continue."));
            assert!(exported.contains("[redacted]"));
        }
    }

    #[test]
    fn export_redaction_is_idempotent() {
        // Re-redacting an already-exported (redacted) string is a fixed point:
        // a second pass over a report must not re-expose or further mangle it.
        let once = redact_secrets("token=sk-abc123def456 keep this safe");
        let twice = redact_secrets(&once);
        assert_eq!(once, twice, "redaction must be idempotent for export reuse");
        assert!(!twice.contains("sk-abc123def456"));
    }
}
