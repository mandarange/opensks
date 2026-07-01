use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use opensks_contracts::{TERMINAL_COMMAND_BLOCK_SCHEMA, TerminalCommandBlock};
use serde::{Deserialize, Serialize};

use crate::redaction::{digest_bytes, redact_command, redact_output_for_summary, redact_path};

static BLOCK_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Cap on in-flight stdout captured per block, both raw (for digesting) and
/// redacted (for summary text), to avoid unbounded memory growth for a single
/// long-running or verbose command.
const MAX_ACTIVE_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCommandBlockSummary {
    pub block_id: String,
    pub session_id: String,
    pub command_redacted: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub stdout_digest: Option<String>,
    pub stderr_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CommandBlockBuilder {
    session_id: String,
    workspace: PathBuf,
    cwd: PathBuf,
    active: Option<ActiveBlock>,
}

#[derive(Debug, Clone)]
struct ActiveBlock {
    block_id: String,
    command_redacted: String,
    started_at_ms: u64,
    /// Raw (pre-redaction) stdout bytes, used to compute a genuine,
    /// reproducible integrity digest.
    stdout_raw: Vec<u8>,
    /// Redacted stdout text, used for human-readable summaries.
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Bytes held back because they may be an incomplete UTF-8 sequence
    /// split across PTY read chunk boundaries.
    pending_utf8: Vec<u8>,
}

impl CommandBlockBuilder {
    pub fn new(
        session_id: impl Into<String>,
        workspace: impl AsRef<Path>,
        cwd: impl AsRef<Path>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            workspace: workspace.as_ref().to_path_buf(),
            cwd: cwd.as_ref().to_path_buf(),
            active: None,
        }
    }

    pub fn observe_input(&mut self, text: &str) -> Option<TerminalCommandBlock> {
        if !text.contains('\n') {
            return None;
        }
        let candidate = text
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        let closed = self.flush(None);
        self.active = Some(ActiveBlock {
            block_id: next_block_id(),
            command_redacted: redact_command(candidate, &self.workspace),
            started_at_ms: now_ms(),
            stdout_raw: Vec::new(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            pending_utf8: Vec::new(),
        });
        closed
    }

    pub fn observe_stdout(&mut self, bytes: &[u8]) {
        if let Some(active) = self.active.as_mut() {
            append_capped(&mut active.stdout_raw, bytes, MAX_ACTIVE_OUTPUT_BYTES);

            active.pending_utf8.extend_from_slice(bytes);
            let decoded = match std::str::from_utf8(&active.pending_utf8) {
                Ok(text) => {
                    let text = text.to_string();
                    active.pending_utf8.clear();
                    text
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    let text = String::from_utf8_lossy(&active.pending_utf8[..valid_up_to])
                        .into_owned();
                    let remainder = active.pending_utf8[valid_up_to..].to_vec();
                    // Only keep buffering the trailing bytes if they could
                    // still be an incomplete (not conclusively invalid)
                    // UTF-8 sequence; otherwise decode lossily now so we
                    // don't buffer garbage forever.
                    if err.error_len().is_none() && remainder.len() <= 3 {
                        active.pending_utf8 = remainder;
                        text
                    } else {
                        active.pending_utf8.clear();
                        text + &String::from_utf8_lossy(&remainder)
                    }
                }
            };
            if !decoded.is_empty() {
                let redacted = redact_output_for_summary(&decoded, &self.workspace);
                append_capped(&mut active.stdout, redacted.as_bytes(), MAX_ACTIVE_OUTPUT_BYTES);
            }
        }
    }

    pub fn flush(&mut self, exit_code: Option<i32>) -> Option<TerminalCommandBlock> {
        let mut active = self.active.take()?;
        if !active.pending_utf8.is_empty() {
            let text = String::from_utf8_lossy(&active.pending_utf8).into_owned();
            active.pending_utf8.clear();
            let redacted = redact_output_for_summary(&text, &self.workspace);
            append_capped(&mut active.stdout, redacted.as_bytes(), MAX_ACTIVE_OUTPUT_BYTES);
        }
        Some(TerminalCommandBlock {
            schema: TERMINAL_COMMAND_BLOCK_SCHEMA.to_string(),
            block_id: active.block_id,
            session_id: self.session_id.clone(),
            cwd_redacted: redact_path(&self.cwd, &self.workspace),
            command_redacted: active.command_redacted,
            started_at_ms: active.started_at_ms,
            finished_at_ms: Some(now_ms()),
            exit_code,
            stdout_digest: Some(digest_bytes(&active.stdout_raw)),
            stderr_digest: if active.stderr.is_empty() {
                None
            } else {
                Some(digest_bytes(&active.stderr))
            },
            redacted: true,
            evidence_refs: vec![format!("terminal:session:{}", self.session_id)],
        })
    }
}

/// Appends `data` to `buf`, stopping (and appending a truncation marker) once
/// `cap` bytes have been accumulated, to bound in-memory growth for a single
/// active block regardless of how much output the underlying command emits.
fn append_capped(buf: &mut Vec<u8>, data: &[u8], cap: usize) {
    if buf.len() >= cap {
        return;
    }
    let remaining = cap - buf.len();
    if data.len() <= remaining {
        buf.extend_from_slice(data);
    } else {
        buf.extend_from_slice(&data[..remaining]);
        buf.extend_from_slice(b"\n<...truncated, output exceeded cap...>\n");
    }
}

fn next_block_id() -> String {
    let count = BLOCK_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("term-block-{}-{count}", now_ms())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redaction::digest_bytes;

    #[test]
    fn stdout_digest_is_stable_across_chunk_boundaries() {
        let raw = b"line one\nline two with plenty of whitespace   here\nfinal line".to_vec();
        let expected = digest_bytes(&raw);

        // Single chunk.
        let mut builder = CommandBlockBuilder::new("session", "/tmp/workspace", "/tmp");
        builder.observe_input("run-command\n");
        builder.observe_stdout(&raw);
        let block = builder.flush(Some(0)).expect("block");
        assert_eq!(block.stdout_digest, Some(expected.clone()));

        // Same bytes split across several arbitrary chunk boundaries.
        let mut builder = CommandBlockBuilder::new("session", "/tmp/workspace", "/tmp");
        builder.observe_input("run-command\n");
        for chunk in raw.chunks(3) {
            builder.observe_stdout(chunk);
        }
        let block = builder.flush(Some(0)).expect("block");
        assert_eq!(block.stdout_digest, Some(expected));
    }

    #[test]
    fn observe_stdout_handles_utf8_split_across_chunks() {
        // "café" — the 'é' is a 2-byte UTF-8 sequence (0xC3 0xA9). Split the
        // bytes so the multi-byte character straddles two PTY read chunks.
        let bytes = "café\n".as_bytes().to_vec();
        assert!(bytes.len() > 2);
        let split_at = bytes.len() - 2;
        let (first, second) = bytes.split_at(split_at);

        let mut builder = CommandBlockBuilder::new("session", "/tmp/workspace", "/tmp");
        builder.observe_input("run-command\n");
        builder.observe_stdout(first);
        builder.observe_stdout(second);
        let active = builder.active.as_ref().expect("active block");
        assert!(active.pending_utf8.is_empty());
        // Redaction of the summary text collapses whitespace, so compare
        // against the redacted (not raw) expected form; the important
        // assertion is that "é" was decoded as one character, not corrupted
        // into replacement characters by a chunk-boundary split.
        assert_eq!(String::from_utf8_lossy(&active.stdout), "café");
        assert_eq!(active.stdout_raw, bytes);
    }
}
