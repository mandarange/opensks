use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::{CliError, CliOutput, file_output, file_usage};

/// Options for the `file` verb. `path` is workspace-relative; `workspace`
/// defaults to the process `cwd`.
#[derive(Debug, Default)]
struct FileCommandOptions {
    workspace: Option<PathBuf>,
    path: Option<String>,
    expected_hash: Option<String>,
    expected_mtime: Option<u64>,
    stdin: bool,
}

/// `opensks file open|save|stat` — the sanctioned editor read/write path over a
/// canonical workspace (PR-032). Every success serializes a typed JSON document;
/// every guard failure emits a content-free `opensks.file-error.v1` JSON and
/// returns `CliError::Invalid` so the process exits nonzero.
pub(crate) fn run_file_command(args: &[String], cwd: &Path) -> Result<CliOutput, CliError> {
    // The save subcommand reads its new content from stdin; the read is injected
    // here so the command logic stays testable without a live stdin.
    run_file_command_with_input(args, cwd, read_stdin_to_string)
}

/// Inner implementation of [`run_file_command`] with the stdin read injected as
/// `read_input`, invoked only on the `save` path after its flags validate.
pub(crate) fn run_file_command_with_input<F>(
    args: &[String],
    cwd: &Path,
    read_input: F,
) -> Result<CliOutput, CliError>
where
    F: FnOnce() -> Result<String, CliError>,
{
    let Some(subcommand) = args.first() else {
        return Ok(CliOutput {
            stdout: file_usage().to_string(),
        });
    };
    if subcommand == "--help" || subcommand == "-h" || subcommand == "help" {
        return Ok(CliOutput {
            stdout: file_usage().to_string(),
        });
    }

    let options = parse_file_options(&args[1..])?;
    let workspace = options
        .workspace
        .clone()
        .unwrap_or_else(|| cwd.to_path_buf());
    let relative = require_file_field(options.path.as_deref(), "--path")?;

    // Open the service over the canonical workspace. A bad workspace root is a
    // configuration error, not a per-file guard verdict, so it is reported as a
    // usage/invalid error rather than a `file-error.v1` payload.
    let service = match opensks_file_service::WorkspaceFileService::open(&workspace) {
        Ok(service) => service,
        Err(error) => {
            return Err(CliError::Invalid(format!(
                "open workspace `{}`: {}",
                workspace.display(),
                error.reason_code()
            )));
        }
    };

    match subcommand.as_str() {
        "open" => match service.open_text(relative) {
            Ok(document) => file_output(&file_document_json(&document)),
            Err(error) => Err(file_error(&error)),
        },
        "stat" => match service.stat(relative) {
            Ok(entry) => file_output(&file_entry_json(&entry)),
            Err(error) => Err(file_error(&error)),
        },
        "save" => {
            let expected_hash =
                require_file_field(options.expected_hash.as_deref(), "--expected-hash")?;
            if !options.stdin {
                return Err(CliError::Usage(format!(
                    "file save requires `--stdin` (new content is read from stdin)\n\n{}",
                    file_usage()
                )));
            }
            let content = read_input()?;
            let mut request =
                opensks_contracts::SaveTextRequest::new(relative, content, expected_hash);
            request.expected_mtime_ms = options.expected_mtime;
            match service.save_text(&request) {
                Ok(result) => file_output(&file_save_json(&result)),
                Err(error) => Err(file_error(&error)),
            }
        }
        "diff" => {
            if !options.stdin {
                return Err(CliError::Usage(format!(
                    "file diff requires `--stdin` (the editor buffer is read from stdin)\n\n{}",
                    file_usage()
                )));
            }
            // Read the on-disk file through the hardened service so the diff
            // honors the same guards (escape/secret/binary) as open/save.
            let document = match service.open_text(relative) {
                Ok(document) => document,
                Err(error) => return Err(file_error(&error)),
            };
            let buffer = read_input()?;
            let diff = compute_text_diff(relative, &document.content, &buffer);
            let value = serde_json::to_value(&diff)
                .map_err(|error| CliError::Invalid(format!("serialize text diff: {error}")))?;
            file_output(&value)
        }
        other => Err(CliError::Usage(format!(
            "unknown file subcommand `{other}`\n\n{}",
            file_usage()
        ))),
    }
}

/// Line-level diff of the editor's `buffer` against the `on_disk` content.
///
/// A simple longest-common-subsequence (LCS) walk over lines groups runs of
/// `-`/`+` lines into [`opensks_contracts::DiffHunk`]s. Pure deletions are
/// `Removed`, pure insertions are `Added`, and any block touching both is
/// `Changed`. The result never carries unchanged lines, only the changed ones.
fn compute_text_diff(path: &str, on_disk: &str, buffer: &str) -> opensks_contracts::TextDiff {
    let old_lines: Vec<&str> = split_diff_lines(on_disk);
    let new_lines: Vec<&str> = split_diff_lines(buffer);
    let ops = lcs_diff(&old_lines, &new_lines);
    let hunks = group_diff_hunks(&ops, &old_lines, &new_lines);
    opensks_contracts::TextDiff::new(path, hunks)
}

/// Split text into lines for diffing. A single trailing newline is treated as a
/// terminator (not a spurious trailing empty line); empty input is zero lines.
/// Interior blank lines are preserved.
fn split_diff_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    // `"a\n".split('\n')` yields ["a", ""]; drop that one terminator artifact.
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// A single line-level edit operation produced by the LCS walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    /// Line present in both files (advances both cursors).
    Equal,
    /// Line only on disk (advances the old cursor).
    Remove,
    /// Line only in the buffer (advances the new cursor).
    Add,
}

/// Classic dynamic-programming LCS over lines, emitting an ordered op list.
fn lcs_diff(old_lines: &[&str], new_lines: &[&str]) -> Vec<DiffOp> {
    let rows = old_lines.len();
    let cols = new_lines.len();
    // table[i][j] = LCS length of old_lines[i..] and new_lines[j..].
    let mut table = vec![vec![0usize; cols + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            table[i][j] = if old_lines[i] == new_lines[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < rows && j < cols {
        if old_lines[i] == new_lines[j] {
            ops.push(DiffOp::Equal);
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push(DiffOp::Remove);
            i += 1;
        } else {
            ops.push(DiffOp::Add);
            j += 1;
        }
    }
    while i < rows {
        ops.push(DiffOp::Remove);
        i += 1;
    }
    while j < cols {
        ops.push(DiffOp::Add);
        j += 1;
    }
    ops
}

/// Collapse the flat op list into contiguous hunks of change, tracking 1-based
/// line numbers in both the old and new files.
fn group_diff_hunks(
    ops: &[DiffOp],
    old_lines: &[&str],
    new_lines: &[&str],
) -> Vec<opensks_contracts::DiffHunk> {
    let mut hunks = Vec::new();
    let mut old_index = 0usize; // 0-based cursor into old_lines
    let mut new_index = 0usize; // 0-based cursor into new_lines
    let mut pending: Vec<DiffOp> = Vec::new();
    let mut hunk_old_start = 0usize;
    let mut hunk_new_start = 0usize;

    let flush = |pending: &mut Vec<DiffOp>,
                 hunk_old_start: usize,
                 hunk_new_start: usize,
                 old_lines: &[&str],
                 new_lines: &[&str],
                 old_cursor: usize,
                 new_cursor: usize,
                 hunks: &mut Vec<opensks_contracts::DiffHunk>| {
        if pending.is_empty() {
            return;
        }
        let removed = pending.iter().filter(|op| **op == DiffOp::Remove).count();
        let added = pending.iter().filter(|op| **op == DiffOp::Add).count();
        let kind = match (removed > 0, added > 0) {
            (true, true) => opensks_contracts::DiffHunkKind::Changed,
            (true, false) => opensks_contracts::DiffHunkKind::Removed,
            (false, true) => opensks_contracts::DiffHunkKind::Added,
            (false, false) => return,
        };
        let mut lines = Vec::with_capacity(removed + added);
        for line in &old_lines[hunk_old_start..old_cursor] {
            lines.push(format!("-{line}"));
        }
        for line in &new_lines[hunk_new_start..new_cursor] {
            lines.push(format!("+{line}"));
        }
        hunks.push(opensks_contracts::DiffHunk {
            kind,
            old_start: hunk_old_start + 1,
            old_lines: removed,
            new_start: hunk_new_start + 1,
            new_lines: added,
            lines,
        });
        pending.clear();
    };

    for op in ops {
        match op {
            DiffOp::Equal => {
                flush(
                    &mut pending,
                    hunk_old_start,
                    hunk_new_start,
                    old_lines,
                    new_lines,
                    old_index,
                    new_index,
                    &mut hunks,
                );
                old_index += 1;
                new_index += 1;
            }
            DiffOp::Remove => {
                if pending.is_empty() {
                    hunk_old_start = old_index;
                    hunk_new_start = new_index;
                }
                pending.push(DiffOp::Remove);
                old_index += 1;
            }
            DiffOp::Add => {
                if pending.is_empty() {
                    hunk_old_start = old_index;
                    hunk_new_start = new_index;
                }
                pending.push(DiffOp::Add);
                new_index += 1;
            }
        }
    }
    flush(
        &mut pending,
        hunk_old_start,
        hunk_new_start,
        old_lines,
        new_lines,
        old_index,
        new_index,
        &mut hunks,
    );
    hunks
}

fn parse_file_options(args: &[String]) -> Result<FileCommandOptions, CliError> {
    let mut options = FileCommandOptions::default();
    let mut idx = 0;
    while idx < args.len() {
        let flag = args[idx].as_str();
        match flag {
            "--workspace" => {
                options.workspace = Some(PathBuf::from(file_flag_value(args, idx, flag)?));
                idx += 2;
            }
            "--path" => {
                options.path = Some(file_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--expected-hash" => {
                options.expected_hash = Some(file_flag_value(args, idx, flag)?.to_string());
                idx += 2;
            }
            "--expected-mtime" => {
                options.expected_mtime = Some(file_parse_u64(args, idx, flag)?);
                idx += 2;
            }
            "--stdin" => {
                options.stdin = true;
                idx += 1;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown file argument `{other}`\n\n{}",
                    file_usage()
                )));
            }
        }
    }
    Ok(options)
}

fn file_flag_value<'a>(args: &'a [String], idx: usize, flag: &str) -> Result<&'a str, CliError> {
    args.get(idx + 1).map(String::as_str).ok_or_else(|| {
        CliError::Usage(format!(
            "file flag `{flag}` requires a value\n\n{}",
            file_usage()
        ))
    })
}

fn file_parse_u64(args: &[String], idx: usize, flag: &str) -> Result<u64, CliError> {
    file_flag_value(args, idx, flag)?
        .parse::<u64>()
        .map_err(|_| {
            CliError::Usage(format!(
                "file flag `{flag}` expects a non-negative integer\n\n{}",
                file_usage()
            ))
        })
}

fn require_file_field<'a>(value: Option<&'a str>, flag: &str) -> Result<&'a str, CliError> {
    value.ok_or_else(|| {
        CliError::Usage(format!(
            "file command requires `{flag}`\n\n{}",
            file_usage()
        ))
    })
}

/// Read the full save payload from stdin. The bytes are the new file content and
/// are never echoed into an error message.
fn read_stdin_to_string() -> Result<String, CliError> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    Ok(input)
}

/// Serialize an opened document to the `opensks.text-document.v1` wire shape with
/// the contract's explicit field names (`encoding:"utf-8"`, string `line_ending`,
/// `is_binary:false`).
fn file_document_json(document: &opensks_contracts::TextDocument) -> serde_json::Value {
    serde_json::json!({
        "schema": document.schema,
        "workspace_relative_path": document.workspace_relative_path,
        "content": document.content,
        "content_hash": document.content_hash,
        "encoding": "utf-8",
        "line_ending": document.line_ending.as_str(),
        "byte_size": document.byte_size,
        "is_secret_restricted": document.is_secret_restricted,
        "is_binary": false,
        "on_disk_modification_ms": document.on_disk_modification_ms,
        "permissions_mode": document.permissions_mode,
    })
}

/// Serialize a successful save to the `opensks.save-result.v1` wire shape.
fn file_save_json(result: &opensks_contracts::SaveTextResult) -> serde_json::Value {
    serde_json::json!({
        "schema": "opensks.save-result.v1",
        "workspace_relative_path": result.workspace_relative_path,
        "new_hash": result.new_hash,
        "new_mtime_ms": result.new_mtime_ms,
    })
}

/// Serialize a stat to the `opensks.workspace-entry.v1` wire shape.
fn file_entry_json(entry: &opensks_contracts::WorkspaceEntry) -> serde_json::Value {
    serde_json::json!({
        "schema": entry.schema,
        "workspace_relative_path": entry.workspace_relative_path,
        "byte_size": entry.byte_size,
        "modification_ms": entry.modification_ms,
        "permissions_mode": entry.permissions_mode,
        "content_hash": entry.content_hash,
        "is_secret_restricted": entry.is_secret_restricted,
    })
}

/// Map a `FileServiceError` to the `opensks.file-error.v1` envelope and wrap it
/// in `CliError::Invalid` so the binary prints the JSON and exits nonzero. The
/// message is derived solely from the stable reason code and the
/// workspace-relative path — never from file contents.
fn file_error(error: &opensks_contracts::FileServiceError) -> CliError {
    let body = serde_json::json!({
        "schema": "opensks.file-error.v1",
        "error": {
            "code": error.reason_code(),
            "message": error.to_string(),
        },
    });
    let payload = serde_json::to_string(&body)
        .unwrap_or_else(|_| "{\"schema\":\"opensks.file-error.v1\"}".to_string());
    CliError::Invalid(payload)
}
