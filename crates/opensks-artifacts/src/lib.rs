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
}
