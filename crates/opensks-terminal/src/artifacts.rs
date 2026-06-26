use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use opensks_contracts::TerminalCommandBlock;
use serde_json::json;

use crate::session::TerminalSessionSnapshot;

#[derive(Debug, Clone)]
pub struct TerminalArtifactPaths {
    pub session_dir: PathBuf,
    pub session_json: PathBuf,
    pub events_jsonl: PathBuf,
    pub blocks_jsonl: PathBuf,
    pub output_raw: PathBuf,
    pub daily_log_jsonl: PathBuf,
}

#[derive(Debug, Clone)]
pub struct TerminalArtifactWriter {
    pub paths: TerminalArtifactPaths,
}

impl TerminalArtifactWriter {
    pub fn open(workspace: impl AsRef<Path>, session_id: &str) -> io::Result<Self> {
        let workspace = workspace.as_ref();
        let session_dir = workspace
            .join(".opensks")
            .join("runtime")
            .join("terminal")
            .join("sessions")
            .join(session_id);
        let logs_dir = workspace.join(".opensks").join("logs").join("terminal");
        fs::create_dir_all(&session_dir)?;
        fs::create_dir_all(&logs_dir)?;
        Ok(Self {
            paths: TerminalArtifactPaths {
                session_json: session_dir.join("session.json"),
                events_jsonl: session_dir.join("events.jsonl"),
                blocks_jsonl: session_dir.join("blocks.jsonl"),
                output_raw: session_dir.join("output.raw"),
                session_dir,
                daily_log_jsonl: logs_dir.join(format!("{}.jsonl", utc_date())),
            },
        })
    }

    pub fn write_session_snapshot(&self, snapshot: &TerminalSessionSnapshot) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(snapshot)?;
        atomic_write(&self.paths.session_json, &bytes)
    }

    pub fn append_event(&self, value: &serde_json::Value) -> io::Result<()> {
        append_json_line(&self.paths.events_jsonl, value)?;
        let log_value = json!({
            "schema": "opensks.terminal-local-log.v1",
            "event": value,
        });
        append_json_line(&self.paths.daily_log_jsonl, &log_value)
    }

    pub fn append_block(&self, block: &TerminalCommandBlock) -> io::Result<()> {
        let value = serde_json::to_value(block)?;
        append_json_line(&self.paths.blocks_jsonl, &value)
    }

    pub fn append_raw_output(&self, bytes: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.paths.output_raw)?;
        file.write_all(bytes)
    }
}

fn append_json_line(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

fn utc_date() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 86_400)
        .unwrap_or_default() as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m as u32, d as u32)
}
