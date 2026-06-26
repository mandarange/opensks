use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use opensks_contracts::TerminalEnvPolicy;
use portable_pty::{Child, CommandBuilder, MasterPty, native_pty_system};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::artifacts::TerminalArtifactWriter;
use crate::block::{CommandBlockBuilder, now_ms};
use crate::pty::{normalize_cols, normalize_rows, pty_size};
use crate::redaction::digest_bytes;

pub const MAX_OUTPUT_CHUNK_BYTES: usize = 64 * 1024;
pub const READ_TIMEOUT_MS: u64 = 25;
pub const GRACEFUL_STOP_MS: u64 = 750;

#[derive(Debug, Clone)]
pub struct TerminalSessionConfig {
    pub session_id: String,
    pub cwd: PathBuf,
    pub shell: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    pub env_policy: TerminalEnvPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalOutputChunk {
    pub session_id: String,
    pub stream: TerminalStreamKind,
    pub bytes: Vec<u8>,
    pub decoded_lossy: String,
    pub received_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalStreamKind {
    Pty,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSessionSnapshot {
    pub session_id: String,
    pub cwd: PathBuf,
    pub shell: PathBuf,
    pub started_at_ms: u64,
    pub stopped_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub status: TerminalSessionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalSessionStatus {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSessionHandle {
    pub session_id: String,
}

#[derive(Debug, Error)]
pub enum TerminalRuntimeError {
    #[error("terminal session already exists: {0}")]
    SessionAlreadyExists(String),
    #[error("terminal session not found: {0}")]
    SessionNotFound(String),
    #[error("terminal session is not running: {0}")]
    SessionNotRunning(String),
    #[error("invalid cwd `{path}`: {reason}")]
    InvalidCwd { path: PathBuf, reason: String },
    #[error("shell not found: {0}")]
    ShellNotFound(PathBuf),
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
    #[error("pty error: {0}")]
    Pty(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct TerminalRuntime {
    workspace: PathBuf,
    sessions: Mutex<HashMap<String, TerminalSession>>,
}

struct TerminalSession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send>,
    output: Receiver<Vec<u8>>,
    artifacts: TerminalArtifactWriter,
    block_builder: CommandBlockBuilder,
    snapshot: TerminalSessionSnapshot,
}

impl TerminalRuntime {
    pub fn new(workspace: impl AsRef<Path>) -> Self {
        Self {
            workspace: workspace.as_ref().to_path_buf(),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn start_session(
        &self,
        config: TerminalSessionConfig,
    ) -> Result<TerminalSessionHandle, TerminalRuntimeError> {
        #[cfg(windows)]
        {
            let _ = config;
            return Err(TerminalRuntimeError::UnsupportedPlatform(
                "windows PTY runtime is not implemented".to_string(),
            ));
        }

        #[cfg(not(windows))]
        {
            validate_cwd(&config.cwd)?;
            let shell = resolve_shell(config.shell.as_deref())?;
            let cols = normalize_cols(config.cols);
            let rows = normalize_rows(config.rows);
            let started_at_ms = now_ms();
            let snapshot = TerminalSessionSnapshot {
                session_id: config.session_id.clone(),
                cwd: config.cwd.clone(),
                shell: shell.clone(),
                started_at_ms,
                stopped_at_ms: None,
                exit_code: None,
                status: TerminalSessionStatus::Starting,
            };

            let artifacts = TerminalArtifactWriter::open(&self.workspace, &config.session_id)?;
            artifacts.write_session_snapshot(&snapshot)?;
            artifacts.append_event(&json!({
                "type": "session_starting",
                "session_id": config.session_id,
                "cols": cols,
                "rows": rows,
                "env_policy": format!("{:?}", config.env_policy),
                "at_ms": started_at_ms,
            }))?;

            let pty_system = native_pty_system();
            let pair = pty_system
                .openpty(pty_size(cols, rows))
                .map_err(|error| TerminalRuntimeError::Pty(error.to_string()))?;

            let mut command = CommandBuilder::new(shell.as_os_str());
            command.cwd(&config.cwd);
            apply_env_policy(&mut command, &config.env_policy, &shell);

            let child = pair
                .slave
                .spawn_command(command)
                .map_err(|error| TerminalRuntimeError::Pty(error.to_string()))?;
            let reader = pair
                .master
                .try_clone_reader()
                .map_err(|error| TerminalRuntimeError::Pty(error.to_string()))?;
            let writer = pair
                .master
                .take_writer()
                .map_err(|error| TerminalRuntimeError::Pty(error.to_string()))?;
            let output = spawn_reader(reader);

            let mut running_snapshot = snapshot;
            running_snapshot.status = TerminalSessionStatus::Running;
            artifacts.write_session_snapshot(&running_snapshot)?;
            artifacts.append_event(&json!({
                "type": "session_started",
                "session_id": running_snapshot.session_id,
                "at_ms": now_ms(),
            }))?;

            let session_id = running_snapshot.session_id.clone();
            let session = TerminalSession {
                master: pair.master,
                writer,
                child,
                output,
                artifacts,
                block_builder: CommandBlockBuilder::new(
                    session_id.clone(),
                    &self.workspace,
                    &config.cwd,
                ),
                snapshot: running_snapshot,
            };
            let mut sessions = self
                .sessions
                .lock()
                .expect("terminal session lock poisoned");
            if sessions.contains_key(&session_id) {
                return Err(TerminalRuntimeError::SessionAlreadyExists(session_id));
            }
            sessions.insert(session_id.clone(), session);
            Ok(TerminalSessionHandle { session_id })
        }
    }

    pub fn write_input(&self, session_id: &str, text: &str) -> Result<(), TerminalRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("terminal session lock poisoned");
        let session = running_session_mut(&mut sessions, session_id)?;
        session.writer.write_all(text.as_bytes())?;
        session.writer.flush()?;
        session.artifacts.append_event(&json!({
            "type": "input",
            "session_id": session_id,
            "raw_local_only": true,
            "bytes": text.as_bytes().len(),
            "contains_newline": text.contains('\n'),
            "at_ms": now_ms(),
        }))?;
        if let Some(block) = session.block_builder.observe_input(text) {
            session.artifacts.append_block(&block)?;
        }
        Ok(())
    }

    pub fn read_available(
        &self,
        session_id: &str,
    ) -> Result<Vec<TerminalOutputChunk>, TerminalRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("terminal session lock poisoned");
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| TerminalRuntimeError::SessionNotFound(session_id.to_string()))?;
        let mut chunks = Vec::new();
        if let Ok(bytes) = session
            .output
            .recv_timeout(Duration::from_millis(READ_TIMEOUT_MS))
        {
            append_output_chunks(session, session_id, bytes, &mut chunks)?;
        }
        while let Ok(bytes) = session.output.try_recv() {
            append_output_chunks(session, session_id, bytes, &mut chunks)?;
        }
        if let Some(status) = session.child.try_wait()? {
            session.snapshot.status = TerminalSessionStatus::Exited;
            session.snapshot.stopped_at_ms = Some(now_ms());
            session.snapshot.exit_code = Some(status.exit_code() as i32);
            session
                .artifacts
                .write_session_snapshot(&session.snapshot)?;
        }
        Ok(chunks)
    }

    pub fn resize(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), TerminalRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("terminal session lock poisoned");
        let session = running_session_mut(&mut sessions, session_id)?;
        let cols = normalize_cols(cols);
        let rows = normalize_rows(rows);
        session
            .master
            .resize(pty_size(cols, rows))
            .map_err(|error| TerminalRuntimeError::Pty(error.to_string()))?;
        session.artifacts.append_event(&json!({
            "type": "resize",
            "session_id": session_id,
            "cols": cols,
            "rows": rows,
            "at_ms": now_ms(),
        }))?;
        Ok(())
    }

    pub fn stop(&self, session_id: &str) -> Result<TerminalSessionSnapshot, TerminalRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("terminal session lock poisoned");
        let mut session = sessions
            .remove(session_id)
            .ok_or_else(|| TerminalRuntimeError::SessionNotFound(session_id.to_string()))?;

        session.snapshot.status = TerminalSessionStatus::Stopping;
        session
            .artifacts
            .write_session_snapshot(&session.snapshot)?;
        let _ = session.writer.write_all(b"exit\n");
        let _ = session.writer.flush();

        let deadline = Instant::now() + Duration::from_millis(GRACEFUL_STOP_MS);
        let mut exit_code = None;
        while Instant::now() < deadline {
            if let Some(status) = session.child.try_wait()? {
                exit_code = Some(status.exit_code() as i32);
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if exit_code.is_none() {
            session.child.kill()?;
            let status = session.child.wait()?;
            exit_code = Some(status.exit_code() as i32);
        }

        if let Some(block) = session.block_builder.flush(exit_code) {
            session.artifacts.append_block(&block)?;
        }
        session.snapshot.status = TerminalSessionStatus::Exited;
        session.snapshot.exit_code = exit_code;
        session.snapshot.stopped_at_ms = Some(now_ms());
        session
            .artifacts
            .write_session_snapshot(&session.snapshot)?;
        session.artifacts.append_event(&json!({
            "type": "session_stopped",
            "session_id": session_id,
            "exit_code": exit_code,
            "at_ms": session.snapshot.stopped_at_ms,
        }))?;
        Ok(session.snapshot)
    }
}

fn validate_cwd(cwd: &Path) -> Result<(), TerminalRuntimeError> {
    if !cwd.exists() {
        return Err(TerminalRuntimeError::InvalidCwd {
            path: cwd.to_path_buf(),
            reason: "path does not exist".to_string(),
        });
    }
    if cwd.is_file() {
        return Err(TerminalRuntimeError::InvalidCwd {
            path: cwd.to_path_buf(),
            reason: "path is a file".to_string(),
        });
    }
    Ok(())
}

fn resolve_shell(config_shell: Option<&Path>) -> Result<PathBuf, TerminalRuntimeError> {
    let candidates = config_shell
        .map(|shell| vec![shell.to_path_buf()])
        .unwrap_or_else(|| {
            let mut candidates = Vec::new();
            if let Some(shell) = std::env::var_os("SHELL") {
                candidates.push(PathBuf::from(shell));
            }
            candidates.push(PathBuf::from("/bin/zsh"));
            candidates.push(PathBuf::from("/bin/sh"));
            candidates
        });
    candidates
        .into_iter()
        .find(|shell| shell.exists() && shell.is_file())
        .ok_or_else(|| TerminalRuntimeError::ShellNotFound(PathBuf::from("<unresolved>")))
}

fn apply_env_policy(command: &mut CommandBuilder, policy: &TerminalEnvPolicy, shell: &Path) {
    match policy {
        TerminalEnvPolicy::Minimal => {
            command.env_clear();
            for key in ["TERM", "PATH", "HOME", "SHELL", "LANG"] {
                if key == "SHELL" {
                    command.env(key, shell.as_os_str());
                } else if let Some(value) = std::env::var_os(key) {
                    command.env(key, value);
                }
            }
        }
        TerminalEnvPolicy::InheritSafe
        | TerminalEnvPolicy::DenySecrets
        | TerminalEnvPolicy::Unknown => {
            for (key, _) in std::env::vars_os() {
                if secret_key_like(&key) {
                    command.env_remove(key);
                }
            }
        }
    }
}

fn secret_key_like(key: &OsString) -> bool {
    let key = key.to_string_lossy().to_ascii_lowercase();
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("passwd")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("private")
        || key.contains("credential")
        || key.contains("auth")
}

fn spawn_reader(mut reader: Box<dyn Read + Send>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = vec![0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn append_output_chunks(
    session: &mut TerminalSession,
    session_id: &str,
    bytes: Vec<u8>,
    chunks: &mut Vec<TerminalOutputChunk>,
) -> Result<(), TerminalRuntimeError> {
    session.artifacts.append_raw_output(&bytes)?;
    session.block_builder.observe_stdout(&bytes);
    session.artifacts.append_event(&json!({
        "type": "output",
        "session_id": session_id,
        "stream": "pty",
        "bytes": bytes.len(),
        "digest": digest_bytes(&bytes),
        "raw_local_only": true,
        "at_ms": now_ms(),
    }))?;
    for chunk in bytes.chunks(MAX_OUTPUT_CHUNK_BYTES) {
        chunks.push(TerminalOutputChunk {
            session_id: session_id.to_string(),
            stream: TerminalStreamKind::Pty,
            bytes: chunk.to_vec(),
            decoded_lossy: String::from_utf8_lossy(chunk).to_string(),
            received_at_ms: now_ms(),
        });
    }
    Ok(())
}

fn running_session_mut<'a>(
    sessions: &'a mut HashMap<String, TerminalSession>,
    session_id: &str,
) -> Result<&'a mut TerminalSession, TerminalRuntimeError> {
    let session = sessions
        .get_mut(session_id)
        .ok_or_else(|| TerminalRuntimeError::SessionNotFound(session_id.to_string()))?;
    if session.snapshot.status != TerminalSessionStatus::Running {
        return Err(TerminalRuntimeError::SessionNotRunning(
            session_id.to_string(),
        ));
    }
    Ok(session)
}
