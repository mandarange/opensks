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
    let tokens: Vec<&str> = input.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::with_capacity(tokens.len());
    let mut redact_next = false;
    for token in tokens {
        if redact_next {
            out.push("[redacted]");
            redact_next = false;
            continue;
        }
        if is_sensitive_label(token) {
            redact_next = true;
        }
        out.push(if looks_secret(token) {
            "[redacted]"
        } else {
            token
        });
    }
    out.join(" ")
}

/// True when `token` is a label (e.g. `Authorization:`, `token=`, `Bearer`)
/// indicating that the *next* whitespace-delimited token is the sensitive
/// value, even if the label token itself is also redacted independently.
fn is_sensitive_label(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let lower = lower.trim_end_matches([':', '=']);
    lower == "bearer"
        || lower == "basic"
        || lower == "authorization"
        || lower == "token"
        || lower == "secret"
        || lower == "password"
}

fn looks_secret(token: &str) -> bool {
    if looks_secret_core(token) {
        return true;
    }
    // `KEY=VALUE` / `KEY:VALUE` (e.g. `key=(sk-test123)` or `password: …`):
    // judge the value part after the last separator too, so a secret nested
    // behind punctuation on a `key=(...)`-style token is still caught.
    for sep in ['=', ':'] {
        if let Some((_, value)) = token.rsplit_once(sep) {
            if !value.is_empty() && looks_secret_core(value) {
                return true;
            }
        }
    }
    false
}

fn looks_secret_core(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    let lower = token.to_ascii_lowercase();
    let lower_core = core.to_ascii_lowercase();

    // Fast-path: existing whole-word keyword checks, run against both the
    // raw token and its trimmed core so punctuation-wrapped tokens still hit.
    if lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("authorization:")
        || lower_core.contains("secret")
        || lower_core.contains("token")
        || lower_core.contains("password")
        || token.contains("BEGIN_PRIVATE_KEY")
        || core.contains("BEGIN_PRIVATE_KEY")
    {
        return true;
    }

    // Known provider/key prefixes (case-insensitive) on the trimmed core.
    const PREFIXES: [&str; 13] = [
        "sk-",
        "sk_",
        "ghp_",
        "gho_",
        "ghs_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "aws_",
        "akia",
        "asia",
        "aiza",
    ];
    if PREFIXES.iter().any(|p| lower_core.starts_with(p)) {
        return true;
    }

    // High-entropy fallback: long mixed alphanumeric tokens (catches JWTs
    // and generic unlabeled random tokens).
    //
    // Deliberately excludes `-`: known hyphenated secret shapes (`sk-...`,
    // `glpat-...`, `xoxb-...`) are already caught by the prefix list above,
    // so this fallback doesn't need to match hyphens; excluding them keeps
    // it from matching this codebase's kebab-case internal identifiers
    // (work item ids, artifact refs — e.g. `turn-role-<hex>-0-planning`).
    //
    // Also requires at least one non-hex letter (g-z, or any uppercase):
    // this codebase's bare, non-kebab-case internal ids (turn/run ids,
    // content hashes) are lowercase hex (`0-9a-f` only), while real
    // high-entropy secrets/JWTs are base64-ish and almost always contain a
    // letter outside the hex range. Both classes must round-trip through the
    // event journal unredacted for later equality/lookup checks to keep
    // working.
    if core.len() >= 28
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && core.chars().any(|c| c.is_ascii_digit())
        && core
            .chars()
            .any(|c| c.is_ascii_alphabetic() && !c.is_ascii_hexdigit())
    {
        return true;
    }

    false
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

    // ======================================================================
    // Redaction hardening — realistic credential formats that must not
    // survive export (AWS keys, GitHub PATs, JWTs, punctuation-wrapped
    // `sk-` secrets, and label-prefixed secrets like `Authorization: Bearer`).
    // ======================================================================

    #[test]
    fn redaction_strips_aws_access_key() {
        let redacted = redact_secrets("aws key is AKIAIOSFODNN7EXAMPLE for the export");
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn redaction_strips_github_pat() {
        let redacted = redact_secrets("use token ghp_1234567890abcdef1234567890abcdef1234 to auth");
        assert!(!redacted.contains("ghp_1234567890abcdef1234567890abcdef1234"));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn redaction_strips_jwt() {
        let redacted = redact_secrets(
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        );
        assert!(!redacted.contains(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"
        ));
        assert!(redacted.contains("[redacted]"));
    }

    #[test]
    fn redaction_strips_punctuation_wrapped_sk_secret() {
        let redacted = redact_secrets("(sk-test123)");
        assert!(!redacted.contains("sk-test123"));

        let redacted = redact_secrets("key=(sk-test123)");
        assert!(!redacted.contains("sk-test123"));
    }
}
