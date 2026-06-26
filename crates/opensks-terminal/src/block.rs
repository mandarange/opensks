use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use opensks_contracts::{TERMINAL_COMMAND_BLOCK_SCHEMA, TerminalCommandBlock};
use serde::{Deserialize, Serialize};

use crate::redaction::{digest_bytes, redact_command, redact_output_for_summary, redact_path};

static BLOCK_COUNTER: AtomicU64 = AtomicU64::new(1);

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
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
        closed
    }

    pub fn observe_stdout(&mut self, bytes: &[u8]) {
        if let Some(active) = self.active.as_mut() {
            let redacted =
                redact_output_for_summary(&String::from_utf8_lossy(bytes), &self.workspace);
            active.stdout.extend_from_slice(redacted.as_bytes());
        }
    }

    pub fn flush(&mut self, exit_code: Option<i32>) -> Option<TerminalCommandBlock> {
        let active = self.active.take()?;
        Some(TerminalCommandBlock {
            schema: TERMINAL_COMMAND_BLOCK_SCHEMA.to_string(),
            block_id: active.block_id,
            session_id: self.session_id.clone(),
            cwd_redacted: redact_path(&self.cwd, &self.workspace),
            command_redacted: active.command_redacted,
            started_at_ms: active.started_at_ms,
            finished_at_ms: Some(now_ms()),
            exit_code,
            stdout_digest: Some(digest_bytes(&active.stdout)),
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
