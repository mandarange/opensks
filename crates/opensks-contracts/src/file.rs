//! Safe workspace file-service DTOs (PR-031).
//!
//! These are the typed request/response contracts for the hardened workspace
//! file service (open / stat / save / watch). The service implementation lives
//! in the `opensks-file-service` crate; this module owns only the wire shapes
//! and the stable error taxonomy so that the daemon and editor (PR-032) share a
//! single source of truth.
//!
//! Invariant: error values never carry file contents. `FileServiceError`
//! variants only ever reference workspace-relative paths, byte sizes, and
//! stable reason codes — never the bytes being read or written.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const TEXT_DOCUMENT_SCHEMA: &str = "opensks.text-document.v1";
pub const OPEN_TEXT_REQUEST_SCHEMA: &str = "opensks.open-text-request.v1";
pub const SAVE_TEXT_REQUEST_SCHEMA: &str = "opensks.save-text-request.v1";
pub const SAVE_TEXT_RESULT_SCHEMA: &str = "opensks.save-text-result.v1";
pub const STAT_REQUEST_SCHEMA: &str = "opensks.stat-request.v1";
pub const WORKSPACE_ENTRY_SCHEMA: &str = "opensks.workspace-entry.v1";

/// Text encoding of an opened document. PR-031 only supports UTF-8; the enum is
/// kept open so future encodings can be added without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextEncoding {
    Utf8,
}

impl TextEncoding {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Utf8 => "utf8",
        }
    }
}

/// Detected dominant line ending of an opened document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LineEnding {
    /// `\n`
    Lf,
    /// `\r\n`
    Crlf,
}

impl LineEnding {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Lf => "lf",
            Self::Crlf => "crlf",
        }
    }
}

/// Request to open a workspace text document for editing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OpenTextRequest {
    pub schema: String,
    /// Path relative to the workspace root. Absolute paths and any `..`
    /// component are rejected by the service.
    pub workspace_relative_path: String,
}

impl OpenTextRequest {
    pub fn new(workspace_relative_path: impl Into<String>) -> Self {
        Self {
            schema: OPEN_TEXT_REQUEST_SCHEMA.to_string(),
            workspace_relative_path: workspace_relative_path.into(),
        }
    }
}

/// An opened text document plus the metadata needed for safe save conflict
/// detection. `content_hash` and `on_disk_modification_ms` together form the
/// optimistic-concurrency baseline echoed back on save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TextDocument {
    pub schema: String,
    pub workspace_relative_path: String,
    pub content: String,
    pub content_hash: String,
    pub encoding: TextEncoding,
    pub line_ending: LineEnding,
    pub byte_size: u64,
    pub on_disk_modification_ms: u64,
    /// True when the path matches a secret-looking name. Such paths are blocked
    /// for open/save; the flag exists so the UI can explain the refusal.
    pub is_secret_restricted: bool,
    /// Unix permission bits (e.g. `0o644`) of the on-disk file.
    pub permissions_mode: u32,
}

/// Optional override directing the service to overwrite despite a detected
/// on-disk change. The editor must surface a conflict and obtain explicit user
/// intent before sending this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    /// Overwrite the on-disk file even though it changed since baseline.
    OverwriteOnDiskChanges,
}

/// Request to save edited text back to a workspace document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SaveTextRequest {
    pub schema: String,
    pub workspace_relative_path: String,
    pub content: String,
    /// Hash of the content the editor last read; required for conflict
    /// detection.
    pub expected_baseline_hash: String,
    /// Optional mtime the editor last observed. When present it is compared
    /// alongside the baseline hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mtime_ms: Option<u64>,
    /// Optional override to force a write past a detected on-disk change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict_resolution: Option<ConflictResolution>,
}

impl SaveTextRequest {
    pub fn new(
        workspace_relative_path: impl Into<String>,
        content: impl Into<String>,
        expected_baseline_hash: impl Into<String>,
    ) -> Self {
        Self {
            schema: SAVE_TEXT_REQUEST_SCHEMA.to_string(),
            workspace_relative_path: workspace_relative_path.into(),
            content: content.into(),
            expected_baseline_hash: expected_baseline_hash.into(),
            expected_mtime_ms: None,
            conflict_resolution: None,
        }
    }
}

/// Result of a successful save: the new baseline for the next edit cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SaveTextResult {
    pub schema: String,
    pub workspace_relative_path: String,
    pub new_hash: String,
    pub new_mtime_ms: u64,
    pub byte_size: u64,
}

/// Request to stat a workspace entry without reading its contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatRequest {
    pub schema: String,
    pub workspace_relative_path: String,
}

impl StatRequest {
    pub fn new(workspace_relative_path: impl Into<String>) -> Self {
        Self {
            schema: STAT_REQUEST_SCHEMA.to_string(),
            workspace_relative_path: workspace_relative_path.into(),
        }
    }
}

/// Lightweight metadata about a workspace entry (no contents).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceEntry {
    pub schema: String,
    pub workspace_relative_path: String,
    pub byte_size: u64,
    pub modification_ms: u64,
    pub permissions_mode: u32,
    /// Best-effort content hash of the on-disk bytes; empty when the entry was
    /// stat-only and not hashed.
    pub content_hash: String,
    pub is_secret_restricted: bool,
}

/// Stable error taxonomy for the workspace file service.
///
/// Each variant maps to a stable reason code via [`FileServiceError::reason_code`]
/// and, critically, never embeds file contents. The `Display`/`Debug` output of
/// every variant is content-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "code")]
pub enum FileServiceError {
    /// The supplied path was syntactically invalid (empty, non-UTF-8, root,
    /// current-dir only, etc.).
    WorkspacePathInvalid { workspace_relative_path: String },
    /// The path escaped the workspace root (absolute, `..`, or canonicalized
    /// outside the workspace prefix).
    WorkspacePathEscape { workspace_relative_path: String },
    /// A symlink was encountered on the resolved path. Symlinks are rejected to
    /// avoid escaping the workspace via link targets and TOCTOU swaps.
    WorkspaceSymlinkRejected { workspace_relative_path: String },
    /// No regular file exists at the resolved path.
    FileNotFound { workspace_relative_path: String },
    /// The file is not a regular text file (binary content, directory, device,
    /// socket, or FIFO).
    FileBinary { workspace_relative_path: String },
    /// The file exceeds the maximum editable size.
    FileTooLarge {
        workspace_relative_path: String,
        byte_size: u64,
        max_bytes: u64,
    },
    /// The file contents are not valid in any supported encoding.
    FileEncodingUnsupported { workspace_relative_path: String },
    /// The path matches a secret-looking name and is restricted from editing.
    FileSecretRestricted { workspace_relative_path: String },
    /// The on-disk file changed since the editor's baseline; a conflict must be
    /// resolved before saving.
    FileChangedOnDisk { workspace_relative_path: String },
    /// The atomic temp-write-then-rename sequence failed.
    FileAtomicReplaceFailed {
        workspace_relative_path: String,
        reason: String,
    },
    /// The operation was denied by filesystem permissions.
    FilePermissionDenied { workspace_relative_path: String },
}

impl FileServiceError {
    /// Stable machine-readable reason code (the directive's contract).
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::WorkspacePathInvalid { .. } => "workspace_path_invalid",
            Self::WorkspacePathEscape { .. } => "workspace_path_escape",
            Self::WorkspaceSymlinkRejected { .. } => "workspace_symlink_rejected",
            Self::FileNotFound { .. } => "file_not_found",
            Self::FileBinary { .. } => "file_binary",
            Self::FileTooLarge { .. } => "file_too_large",
            Self::FileEncodingUnsupported { .. } => "file_encoding_unsupported",
            Self::FileSecretRestricted { .. } => "file_secret_restricted",
            Self::FileChangedOnDisk { .. } => "file_changed_on_disk",
            Self::FileAtomicReplaceFailed { .. } => "file_atomic_replace_failed",
            Self::FilePermissionDenied { .. } => "file_permission_denied",
        }
    }

    /// The workspace-relative path the error concerns.
    pub fn workspace_relative_path(&self) -> &str {
        match self {
            Self::WorkspacePathInvalid {
                workspace_relative_path,
            }
            | Self::WorkspacePathEscape {
                workspace_relative_path,
            }
            | Self::WorkspaceSymlinkRejected {
                workspace_relative_path,
            }
            | Self::FileNotFound {
                workspace_relative_path,
            }
            | Self::FileBinary {
                workspace_relative_path,
            }
            | Self::FileTooLarge {
                workspace_relative_path,
                ..
            }
            | Self::FileEncodingUnsupported {
                workspace_relative_path,
            }
            | Self::FileSecretRestricted {
                workspace_relative_path,
            }
            | Self::FileChangedOnDisk {
                workspace_relative_path,
            }
            | Self::FileAtomicReplaceFailed {
                workspace_relative_path,
                ..
            }
            | Self::FilePermissionDenied {
                workspace_relative_path,
            } => workspace_relative_path,
        }
    }
}

impl std::fmt::Display for FileServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileTooLarge {
                workspace_relative_path,
                byte_size,
                max_bytes,
            } => write!(
                f,
                "{}: {} ({byte_size} bytes exceeds max {max_bytes} bytes)",
                self.reason_code(),
                workspace_relative_path
            ),
            Self::FileAtomicReplaceFailed {
                workspace_relative_path,
                reason,
            } => write!(
                f,
                "{}: {} ({reason})",
                self.reason_code(),
                workspace_relative_path
            ),
            other => write!(
                f,
                "{}: {}",
                other.reason_code(),
                other.workspace_relative_path()
            ),
        }
    }
}

impl std::error::Error for FileServiceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_request_roundtrips() {
        let request = OpenTextRequest::new("src/main.rs");
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"workspace_relative_path\":\"src/main.rs\""));
        let decoded: OpenTextRequest = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.workspace_relative_path, "src/main.rs");
    }

    #[test]
    fn save_request_roundtrips_with_optional_fields() {
        let mut request = SaveTextRequest::new("notes.txt", "hello", "fnv1a64:0000");
        request.expected_mtime_ms = Some(42);
        request.conflict_resolution = Some(ConflictResolution::OverwriteOnDiskChanges);
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"expected_mtime_ms\":42"));
        assert!(json.contains("\"conflict_resolution\":\"overwrite_on_disk_changes\""));
        let decoded: SaveTextRequest = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.expected_mtime_ms, Some(42));
        assert_eq!(
            decoded.conflict_resolution,
            Some(ConflictResolution::OverwriteOnDiskChanges)
        );
    }

    #[test]
    fn error_reason_codes_are_stable() {
        let cases: &[(FileServiceError, &str)] = &[
            (
                FileServiceError::WorkspacePathInvalid {
                    workspace_relative_path: "".into(),
                },
                "workspace_path_invalid",
            ),
            (
                FileServiceError::WorkspacePathEscape {
                    workspace_relative_path: "../etc".into(),
                },
                "workspace_path_escape",
            ),
            (
                FileServiceError::WorkspaceSymlinkRejected {
                    workspace_relative_path: "link".into(),
                },
                "workspace_symlink_rejected",
            ),
            (
                FileServiceError::FileNotFound {
                    workspace_relative_path: "missing".into(),
                },
                "file_not_found",
            ),
            (
                FileServiceError::FileBinary {
                    workspace_relative_path: "bin".into(),
                },
                "file_binary",
            ),
            (
                FileServiceError::FileTooLarge {
                    workspace_relative_path: "big".into(),
                    byte_size: 10,
                    max_bytes: 5,
                },
                "file_too_large",
            ),
            (
                FileServiceError::FileEncodingUnsupported {
                    workspace_relative_path: "enc".into(),
                },
                "file_encoding_unsupported",
            ),
            (
                FileServiceError::FileSecretRestricted {
                    workspace_relative_path: ".env".into(),
                },
                "file_secret_restricted",
            ),
            (
                FileServiceError::FileChangedOnDisk {
                    workspace_relative_path: "conflict".into(),
                },
                "file_changed_on_disk",
            ),
            (
                FileServiceError::FileAtomicReplaceFailed {
                    workspace_relative_path: "atomic".into(),
                    reason: "rename".into(),
                },
                "file_atomic_replace_failed",
            ),
            (
                FileServiceError::FilePermissionDenied {
                    workspace_relative_path: "perm".into(),
                },
                "file_permission_denied",
            ),
        ];
        for (error, code) in cases {
            assert_eq!(error.reason_code(), *code);
            // Tagged enum serializes the stable code into the `code` field.
            let json = serde_json::to_string(error).expect("serialize error");
            assert!(json.contains(&format!("\"code\":\"{code}\"")));
        }
    }
}
