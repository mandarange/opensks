//! Hardened workspace file service (PR-031).
//!
//! `WorkspaceFileService` is rooted at a single canonical workspace directory
//! and exposes `open` / `stat` / `save` / `subscribe_changes`. It is the only
//! sanctioned path for the editor (PR-032) to read and write user files. Every
//! operation enforces, in order:
//!
//! 1. **Syntactic path validation** — workspace-relative only; absolute paths
//!    and any `..` component are rejected before any filesystem access.
//! 2. **Canonical containment** — the workspace root is canonicalized once at
//!    construction; each candidate's existing prefix is canonicalized and must
//!    start with the workspace prefix.
//! 3. **Symlink rejection** — `symlink_metadata` is used (never `metadata`),
//!    and a symlink at the target — or any intermediate component — is rejected.
//!    This is also re-checked immediately before the write to close the
//!    open→save TOCTOU window.
//! 4. **Type / size / encoding / secret gates** — only regular UTF-8 text files
//!    under the size limit and not matching a secret-looking name are editable.
//!
//! Saves are atomic: a sibling temp file is created with `O_CREAT | O_EXCL`
//! semantics and `0o600`, bytes are written and fsynced, the original mode is
//! preserved where safe, the temp is renamed over the target, and the parent
//! directory is fsynced. Optimistic concurrency is enforced via the baseline
//! content hash (and optional mtime); a stale baseline yields
//! `file_changed_on_disk` unless an explicit conflict-resolution override is
//! supplied.
//!
//! Invariant: no error value or message ever contains file contents.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use opensks_contracts::{
    FileServiceError, LineEnding, SAVE_TEXT_RESULT_SCHEMA, SaveTextRequest, SaveTextResult,
    TEXT_DOCUMENT_SCHEMA, TextDocument, TextEncoding, WORKSPACE_ENTRY_SCHEMA, WorkspaceEntry,
};

/// Default maximum editable file size: 5 MiB.
pub const DEFAULT_MAX_EDIT_BYTES: u64 = 5 * 1024 * 1024;

/// Number of leading bytes inspected for NUL when sniffing binary content.
const BINARY_SNIFF_BYTES: usize = 8192;

/// A workspace file service rooted at a canonical directory.
#[derive(Debug, Clone)]
pub struct WorkspaceFileService {
    /// Canonicalized workspace root. All resolved paths must live under here.
    root: PathBuf,
    max_edit_bytes: u64,
}

impl WorkspaceFileService {
    /// Open a service rooted at `workspace`, which must already exist and
    /// canonicalize to a directory. Uses the default edit-size limit.
    pub fn open(workspace: &Path) -> Result<Self, FileServiceError> {
        Self::open_with_limit(workspace, DEFAULT_MAX_EDIT_BYTES)
    }

    /// Open a service with an explicit maximum editable size.
    pub fn open_with_limit(
        workspace: &Path,
        max_edit_bytes: u64,
    ) -> Result<Self, FileServiceError> {
        let display = workspace.display().to_string();
        let root = workspace
            .canonicalize()
            .map_err(|error| map_io_error(&display, "workspace_root", error))?;
        if !root.is_dir() {
            return Err(FileServiceError::WorkspacePathInvalid {
                workspace_relative_path: display,
            });
        }
        Ok(Self {
            root,
            max_edit_bytes,
        })
    }

    /// The canonical workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Open a workspace text document for editing.
    pub fn open_text(&self, relative: &str) -> Result<TextDocument, FileServiceError> {
        self.guard_secret(relative)?;
        // `resolve` enforces syntactic validation, symlink rejection, and
        // canonical containment in that order.
        let resolved = self.resolve(relative)?;

        let metadata = fs::symlink_metadata(&resolved)
            .map_err(|error| map_io_error(relative, "stat", error))?;
        if metadata.file_type().is_symlink() {
            return Err(FileServiceError::WorkspaceSymlinkRejected {
                workspace_relative_path: relative.to_string(),
            });
        }
        if !metadata.file_type().is_file() {
            // Directories, devices, sockets, FIFOs are not editable text files.
            return Err(FileServiceError::FileBinary {
                workspace_relative_path: relative.to_string(),
            });
        }
        let byte_size = metadata.len();
        if byte_size > self.max_edit_bytes {
            return Err(FileServiceError::FileTooLarge {
                workspace_relative_path: relative.to_string(),
                byte_size,
                max_bytes: self.max_edit_bytes,
            });
        }

        let bytes = read_regular_file(relative, &resolved)?;
        if has_nul_byte(&bytes) {
            return Err(FileServiceError::FileBinary {
                workspace_relative_path: relative.to_string(),
            });
        }
        let content =
            String::from_utf8(bytes).map_err(|_| FileServiceError::FileEncodingUnsupported {
                workspace_relative_path: relative.to_string(),
            })?;

        Ok(TextDocument {
            schema: TEXT_DOCUMENT_SCHEMA.to_string(),
            workspace_relative_path: relative.to_string(),
            content_hash: content_hash(content.as_bytes()),
            line_ending: detect_line_ending(&content),
            byte_size,
            on_disk_modification_ms: modification_ms(&metadata),
            is_secret_restricted: false,
            permissions_mode: metadata.permissions().mode(),
            encoding: TextEncoding::Utf8,
            content,
        })
    }

    /// Stat a workspace entry without returning its contents.
    pub fn stat(&self, relative: &str) -> Result<WorkspaceEntry, FileServiceError> {
        let resolved = self.resolve(relative)?;
        let metadata = fs::symlink_metadata(&resolved)
            .map_err(|error| map_io_error(relative, "stat", error))?;
        if metadata.file_type().is_symlink() {
            return Err(FileServiceError::WorkspaceSymlinkRejected {
                workspace_relative_path: relative.to_string(),
            });
        }
        Ok(WorkspaceEntry {
            schema: WORKSPACE_ENTRY_SCHEMA.to_string(),
            workspace_relative_path: relative.to_string(),
            byte_size: metadata.len(),
            modification_ms: modification_ms(&metadata),
            permissions_mode: metadata.permissions().mode(),
            content_hash: String::new(),
            is_secret_restricted: looks_secret_path(relative),
        })
    }

    /// Save edited text back to a workspace document atomically.
    pub fn save_text(&self, request: &SaveTextRequest) -> Result<SaveTextResult, FileServiceError> {
        let relative = request.workspace_relative_path.as_str();
        self.guard_secret(relative)?;
        // Re-resolve and re-verify (symlink rejection + canonical containment)
        // immediately before the write to shrink the open→save TOCTOU window.
        let resolved = self.resolve(relative)?;

        let metadata = fs::symlink_metadata(&resolved)
            .map_err(|error| map_io_error(relative, "stat", error))?;
        if metadata.file_type().is_symlink() {
            return Err(FileServiceError::WorkspaceSymlinkRejected {
                workspace_relative_path: relative.to_string(),
            });
        }
        if !metadata.file_type().is_file() {
            return Err(FileServiceError::FileBinary {
                workspace_relative_path: relative.to_string(),
            });
        }

        // Optimistic-concurrency check against the current on-disk bytes.
        let on_disk = read_regular_file(relative, &resolved)?;
        let current_hash = content_hash(&on_disk);
        let override_supplied = request.conflict_resolution.is_some();
        let hash_matches = current_hash == request.expected_baseline_hash;
        let mtime_matches = request
            .expected_mtime_ms
            .map(|expected| expected == modification_ms(&metadata))
            .unwrap_or(true);
        if !override_supplied && (!hash_matches || !mtime_matches) {
            return Err(FileServiceError::FileChangedOnDisk {
                workspace_relative_path: relative.to_string(),
            });
        }

        let new_bytes = request.content.as_bytes();
        let new_byte_size = new_bytes.len() as u64;
        if new_byte_size > self.max_edit_bytes {
            return Err(FileServiceError::FileTooLarge {
                workspace_relative_path: relative.to_string(),
                byte_size: new_byte_size,
                max_bytes: self.max_edit_bytes,
            });
        }

        let target_mode = metadata.permissions().mode();
        self.atomic_replace(relative, &resolved, new_bytes, target_mode)?;

        // Re-scan secrets after writing as a defensive final check, and read the
        // fresh mtime for the new baseline.
        self.guard_secret(relative)?;
        let new_metadata = fs::symlink_metadata(&resolved)
            .map_err(|error| map_io_error(relative, "stat", error))?;

        Ok(SaveTextResult {
            schema: SAVE_TEXT_RESULT_SCHEMA.to_string(),
            workspace_relative_path: relative.to_string(),
            new_hash: content_hash(new_bytes),
            new_mtime_ms: modification_ms(&new_metadata),
            byte_size: new_byte_size,
        })
    }

    /// Create a poll/stat-based change watcher for a workspace path.
    ///
    /// PR-031 ships a typed handle plus hash/mtime-based change detection rather
    /// than a real fs-event backend; the editor (PR-032) can drive it on a
    /// timer. The handle captures the baseline at creation time.
    pub fn subscribe_changes(&self, relative: &str) -> Result<WatchHandle, FileServiceError> {
        let entry = self.stat(relative)?;
        Ok(WatchHandle {
            service: self.clone(),
            workspace_relative_path: relative.to_string(),
            baseline_hash: hash_of(self, relative).unwrap_or_default(),
            baseline_mtime_ms: entry.modification_ms,
        })
    }

    /// Resolve a workspace-relative path to a lexical absolute path under the
    /// root, rejecting absolute paths and any `..`/root component, then enforce
    /// the full hardening order: symlink rejection (specific) before canonical
    /// containment (the catch-all escape verdict).
    fn resolve(&self, relative: &str) -> Result<PathBuf, FileServiceError> {
        let candidate = self.lexical_candidate(relative)?;
        // Symlink rejection runs first so a symlink target that escapes the
        // workspace yields the specific `workspace_symlink_rejected` rather than
        // the generic `workspace_path_escape` from canonicalization.
        self.guard_no_symlink(relative, &candidate)?;
        self.verify_containment(relative, &candidate)?;
        Ok(candidate)
    }

    /// Syntactic-only resolution: reject absolute paths, `..`, and root/prefix
    /// components; return the lexical join under the canonical root. Performs no
    /// filesystem access beyond the in-memory join.
    fn lexical_candidate(&self, relative: &str) -> Result<PathBuf, FileServiceError> {
        if relative.is_empty() {
            return Err(FileServiceError::WorkspacePathInvalid {
                workspace_relative_path: relative.to_string(),
            });
        }
        let rel = Path::new(relative);
        if rel.is_absolute() {
            return Err(FileServiceError::WorkspacePathEscape {
                workspace_relative_path: relative.to_string(),
            });
        }
        // Reject any `..` or root/prefix component and normalize away `.`. We do
        // NOT collapse `..` ourselves — its mere presence is an escape attempt.
        let mut normalized = PathBuf::new();
        for component in rel.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(FileServiceError::WorkspacePathEscape {
                        workspace_relative_path: relative.to_string(),
                    });
                }
            }
        }
        if normalized.as_os_str().is_empty() {
            return Err(FileServiceError::WorkspacePathInvalid {
                workspace_relative_path: relative.to_string(),
            });
        }
        Ok(self.root.join(&normalized))
    }

    /// Canonicalize the deepest existing ancestor of `candidate` and confirm it
    /// is contained within the workspace root.
    fn verify_containment(&self, relative: &str, candidate: &Path) -> Result<(), FileServiceError> {
        let existing = deepest_existing_ancestor(candidate);
        let canonical_existing = existing
            .canonicalize()
            .map_err(|error| map_io_error(relative, "canonicalize", error))?;
        if !canonical_existing.starts_with(&self.root) {
            return Err(FileServiceError::WorkspacePathEscape {
                workspace_relative_path: relative.to_string(),
            });
        }
        Ok(())
    }

    /// Reject if the target itself or any intermediate component on the path is
    /// a symlink. Uses `symlink_metadata` and never follows links.
    fn guard_no_symlink(&self, relative: &str, resolved: &Path) -> Result<(), FileServiceError> {
        let mut current = self.root.clone();
        let suffix = resolved.strip_prefix(&self.root).unwrap_or(resolved);
        for component in suffix.components() {
            if let Component::Normal(part) = component {
                current.push(part);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(FileServiceError::WorkspaceSymlinkRejected {
                            workspace_relative_path: relative.to_string(),
                        });
                    }
                    Ok(_) => {}
                    // A not-yet-existing tail component is fine (e.g. save to a
                    // new file); only existing symlinks are rejected.
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }

    fn guard_secret(&self, relative: &str) -> Result<(), FileServiceError> {
        if looks_secret_path(relative) {
            return Err(FileServiceError::FileSecretRestricted {
                workspace_relative_path: relative.to_string(),
            });
        }
        Ok(())
    }

    /// Write `bytes` to a sibling temp file (`O_CREAT | O_EXCL`, mode `0o600`),
    /// fsync it, preserve the target mode, atomically rename it over `target`,
    /// then fsync the parent directory.
    fn atomic_replace(
        &self,
        relative: &str,
        target: &Path,
        bytes: &[u8],
        target_mode: u32,
    ) -> Result<(), FileServiceError> {
        let parent = target
            .parent()
            .ok_or_else(|| FileServiceError::FileAtomicReplaceFailed {
                workspace_relative_path: relative.to_string(),
                reason: "missing_parent".to_string(),
            })?;
        let temp = temp_sibling(target);

        // O_CREAT | O_EXCL with restrictive 0o600 perms. create_new() fails if
        // the temp already exists, preventing a swap onto an attacker file.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|error| atomic_failed(relative, "create_temp", error))?;
        if let Err(error) = file.write_all(bytes) {
            let _ = fs::remove_file(&temp);
            return Err(atomic_failed(relative, "write_temp", error));
        }
        if let Err(error) = file.sync_all() {
            let _ = fs::remove_file(&temp);
            return Err(atomic_failed(relative, "fsync_temp", error));
        }
        // Preserve the target's permission mode on the replacement where safe.
        if let Err(error) = fs::set_permissions(&temp, fs::Permissions::from_mode(target_mode)) {
            let _ = fs::remove_file(&temp);
            return Err(atomic_failed(relative, "set_mode", error));
        }
        drop(file);

        if let Err(error) = fs::rename(&temp, target) {
            let _ = fs::remove_file(&temp);
            return Err(atomic_failed(relative, "rename", error));
        }

        // fsync the parent directory so the rename is durable.
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// A typed change-watch handle. `poll` reports whether the watched path's
/// content hash or mtime diverged from the baseline captured at subscription.
#[derive(Debug, Clone)]
pub struct WatchHandle {
    service: WorkspaceFileService,
    workspace_relative_path: String,
    baseline_hash: String,
    baseline_mtime_ms: u64,
}

/// Outcome of polling a [`WatchHandle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchChange {
    /// No change detected since the baseline.
    Unchanged,
    /// The file's content hash and/or mtime changed.
    Modified { new_hash: String, new_mtime_ms: u64 },
    /// The watched file no longer exists.
    Removed,
}

impl WatchHandle {
    /// The path this handle watches.
    pub fn workspace_relative_path(&self) -> &str {
        &self.workspace_relative_path
    }

    /// The baseline content hash captured at subscription.
    pub fn baseline_hash(&self) -> &str {
        &self.baseline_hash
    }

    /// Detect an out-of-band change via hash/mtime comparison.
    pub fn poll(&self) -> Result<WatchChange, FileServiceError> {
        match self.service.stat(&self.workspace_relative_path) {
            Ok(entry) => {
                let current_hash =
                    hash_of(&self.service, &self.workspace_relative_path).unwrap_or_default();
                if current_hash != self.baseline_hash
                    || entry.modification_ms != self.baseline_mtime_ms
                {
                    Ok(WatchChange::Modified {
                        new_hash: current_hash,
                        new_mtime_ms: entry.modification_ms,
                    })
                } else {
                    Ok(WatchChange::Unchanged)
                }
            }
            Err(FileServiceError::FileNotFound { .. }) => Ok(WatchChange::Removed),
            Err(error) => Err(error),
        }
    }
}

/// Read the content hash of a workspace file (used for watch baselines).
fn hash_of(service: &WorkspaceFileService, relative: &str) -> Result<String, FileServiceError> {
    let resolved = service.resolve(relative)?;
    let bytes = read_regular_file(relative, &resolved)?;
    Ok(content_hash(&bytes))
}

/// Read a path's bytes, mapping IO failures to content-free errors.
fn read_regular_file(relative: &str, resolved: &Path) -> Result<Vec<u8>, FileServiceError> {
    let mut file = File::open(resolved).map_err(|error| map_io_error(relative, "open", error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| map_io_error(relative, "read", error))?;
    Ok(bytes)
}

/// Walk up `candidate` until an existing ancestor is found; the workspace root
/// always exists so this terminates.
fn deepest_existing_ancestor(candidate: &Path) -> PathBuf {
    let mut current = candidate;
    loop {
        if current.exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return current.to_path_buf(),
        }
    }
}

fn temp_sibling(target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_name = format!(".{file_name}.{}.{stamp}.tmp", std::process::id());
    match target.parent() {
        Some(parent) => parent.join(temp_name),
        None => PathBuf::from(temp_name),
    }
}

fn has_nul_byte(bytes: &[u8]) -> bool {
    let window = bytes.len().min(BINARY_SNIFF_BYTES);
    bytes[..window].contains(&0)
}

fn detect_line_ending(content: &str) -> LineEnding {
    if content.contains("\r\n") {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

fn modification_ms(metadata: &fs::Metadata) -> u64 {
    let secs = metadata.mtime().max(0) as u64;
    let nanos = metadata.mtime_nsec().max(0) as u64;
    secs.saturating_mul(1000) + nanos / 1_000_000
}

/// Stable FNV-1a content hash, matching the repo's existing hashing convention.
fn content_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

/// Secret-looking-name policy, aligned with `opensks-git`.
fn looks_secret_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".env")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower.contains("id_rsa")
        || lower.contains(".pem")
}

/// Map an IO error to a content-free `FileServiceError`. Only the kind and a
/// stable stage label are surfaced — never the bytes involved.
fn map_io_error(relative: &str, _stage: &str, error: std::io::Error) -> FileServiceError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => FileServiceError::FileNotFound {
            workspace_relative_path: relative.to_string(),
        },
        ErrorKind::PermissionDenied => FileServiceError::FilePermissionDenied {
            workspace_relative_path: relative.to_string(),
        },
        _ => FileServiceError::FileNotFound {
            workspace_relative_path: relative.to_string(),
        },
    }
}

/// Build an atomic-replace failure carrying only a stable stage label.
fn atomic_failed(relative: &str, stage: &str, error: std::io::Error) -> FileServiceError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return FileServiceError::FilePermissionDenied {
            workspace_relative_path: relative.to_string(),
        };
    }
    FileServiceError::FileAtomicReplaceFailed {
        workspace_relative_path: relative.to_string(),
        reason: stage.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct TempWorkspace {
        root: PathBuf,
    }

    impl TempWorkspace {
        fn new(name: &str) -> Self {
            let mut root = std::env::temp_dir();
            root.push(format!(
                "opensks-file-service-{name}-{}-{}",
                std::process::id(),
                unique()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create workspace");
            Self {
                root: root.canonicalize().expect("canonicalize workspace"),
            }
        }

        fn service(&self) -> WorkspaceFileService {
            WorkspaceFileService::open(&self.root).expect("open service")
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(path, contents).expect("write fixture");
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn unique() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    // --- Traversal blocked -------------------------------------------------

    #[test]
    fn traversal_parent_escape_is_rejected() {
        let ws = TempWorkspace::new("traversal-parent");
        let service = ws.service();
        let error = service.open_text("../etc/passwd").expect_err("escape");
        assert_eq!(error.reason_code(), "workspace_path_escape");
    }

    #[test]
    fn traversal_embedded_parent_escape_is_rejected() {
        let ws = TempWorkspace::new("traversal-embedded");
        let service = ws.service();
        let error = service.open_text("a/../../b").expect_err("escape");
        assert_eq!(error.reason_code(), "workspace_path_escape");
    }

    #[test]
    fn traversal_url_encoded_dotdot_is_not_decoded_into_escape() {
        // "%2e%2e/" must be treated as a literal segment, never decoded into
        // "../". It resolves to a missing file, not an escape.
        let ws = TempWorkspace::new("traversal-encoded");
        let service = ws.service();
        let error = service.open_text("%2e%2e/etc/passwd").expect_err("missing");
        assert_eq!(error.reason_code(), "file_not_found");
    }

    #[test]
    fn traversal_absolute_path_is_rejected() {
        let ws = TempWorkspace::new("traversal-absolute");
        let service = ws.service();
        let error = service.open_text("/etc/passwd").expect_err("absolute");
        assert_eq!(error.reason_code(), "workspace_path_escape");
    }

    // --- Symlink / TOCTOU --------------------------------------------------

    #[test]
    fn symlink_inside_workspace_pointing_outside_is_rejected() {
        let ws = TempWorkspace::new("symlink-open");
        // A real outside target so the link is not dangling.
        let mut outside = std::env::temp_dir();
        outside.push(format!(
            "opensks-outside-{}-{}",
            std::process::id(),
            unique()
        ));
        fs::write(&outside, "outside secret\n").expect("outside file");
        symlink(&outside, ws.path("link.txt")).expect("create symlink");

        let service = ws.service();
        let error = service.open_text("link.txt").expect_err("symlink open");
        assert_eq!(error.reason_code(), "workspace_symlink_rejected");

        let save = SaveTextRequest::new("link.txt", "x", content_hash(b"x"));
        let save_error = service.save_text(&save).expect_err("symlink save");
        assert_eq!(save_error.reason_code(), "workspace_symlink_rejected");
        let _ = fs::remove_file(outside);
    }

    #[test]
    fn toctou_regular_file_swapped_to_symlink_before_save_is_rejected() {
        let ws = TempWorkspace::new("toctou");
        ws.write("doc.txt", "original\n");
        let service = ws.service();
        let document = service.open_text("doc.txt").expect("open regular file");

        // Out-of-band: replace the regular file with a symlink to an outside
        // target (the classic open→save swap).
        let mut outside = std::env::temp_dir();
        outside.push(format!(
            "opensks-toctou-{}-{}",
            std::process::id(),
            unique()
        ));
        fs::write(&outside, "attacker\n").expect("outside file");
        fs::remove_file(ws.path("doc.txt")).expect("remove regular");
        symlink(&outside, ws.path("doc.txt")).expect("swap to symlink");

        let save = SaveTextRequest::new("doc.txt", "edited\n", document.content_hash);
        let error = service.save_text(&save).expect_err("toctou save");
        assert_eq!(error.reason_code(), "workspace_symlink_rejected");
        // The outside target must be untouched.
        assert_eq!(
            fs::read_to_string(&outside).expect("outside intact"),
            "attacker\n"
        );
        let _ = fs::remove_file(outside);
    }

    // --- Binary / oversize / secret ----------------------------------------

    #[test]
    fn binary_file_is_rejected() {
        let ws = TempWorkspace::new("binary");
        fs::write(ws.path("blob.bin"), [0x00u8, 0x01, 0x02, 0x00]).expect("binary fixture");
        let service = ws.service();
        let error = service.open_text("blob.bin").expect_err("binary");
        assert_eq!(error.reason_code(), "file_binary");
    }

    #[test]
    fn oversize_file_is_rejected() {
        let ws = TempWorkspace::new("oversize");
        let service = WorkspaceFileService::open_with_limit(&ws.root, 8).expect("service");
        ws.write("big.txt", "0123456789");
        let error = service.open_text("big.txt").expect_err("oversize");
        assert_eq!(error.reason_code(), "file_too_large");
    }

    #[test]
    fn secret_name_is_rejected() {
        let ws = TempWorkspace::new("secret");
        ws.write(".env", "TOKEN=abc\n");
        let service = ws.service();
        let error = service.open_text(".env").expect_err("secret");
        assert_eq!(error.reason_code(), "file_secret_restricted");
    }

    // --- External change / conflict ----------------------------------------

    #[test]
    fn external_change_with_stale_baseline_is_conflict() {
        let ws = TempWorkspace::new("conflict-stale");
        ws.write("notes.txt", "v1\n");
        let service = ws.service();
        let document = service.open_text("notes.txt").expect("open");

        // Modify out-of-band so the on-disk hash diverges from the baseline.
        ws.write("notes.txt", "v2-external\n");

        let save = SaveTextRequest::new("notes.txt", "v3-edited\n", document.content_hash);
        let error = service.save_text(&save).expect_err("conflict");
        assert_eq!(error.reason_code(), "file_changed_on_disk");
        // The on-disk file must be untouched by the rejected save.
        assert_eq!(
            fs::read_to_string(ws.path("notes.txt")).expect("read"),
            "v2-external\n"
        );
    }

    #[test]
    fn save_with_correct_baseline_succeeds() {
        let ws = TempWorkspace::new("conflict-ok");
        ws.write("notes.txt", "v1\n");
        let service = ws.service();
        let document = service.open_text("notes.txt").expect("open");

        let save = SaveTextRequest::new("notes.txt", "v2-edited\n", document.content_hash);
        let result = service.save_text(&save).expect("save ok");
        assert_eq!(result.new_hash, content_hash(b"v2-edited\n"));
        assert_eq!(
            fs::read_to_string(ws.path("notes.txt")).expect("read"),
            "v2-edited\n"
        );
    }

    #[test]
    fn save_with_override_overwrites_external_change() {
        let ws = TempWorkspace::new("conflict-override");
        ws.write("notes.txt", "v1\n");
        let service = ws.service();
        let document = service.open_text("notes.txt").expect("open");
        ws.write("notes.txt", "v2-external\n");

        let mut save = SaveTextRequest::new("notes.txt", "v3-forced\n", document.content_hash);
        save.conflict_resolution =
            Some(opensks_contracts::ConflictResolution::OverwriteOnDiskChanges);
        let result = service.save_text(&save).expect("override save");
        assert_eq!(result.new_hash, content_hash(b"v3-forced\n"));
        assert_eq!(
            fs::read_to_string(ws.path("notes.txt")).expect("read"),
            "v3-forced\n"
        );
    }

    // --- Atomic write preserves content + mode -----------------------------

    #[test]
    fn atomic_save_preserves_content_and_mode() {
        let ws = TempWorkspace::new("atomic-mode");
        ws.write("script.sh", "#!/bin/sh\necho one\n");
        fs::set_permissions(ws.path("script.sh"), fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let service = ws.service();
        let document = service.open_text("script.sh").expect("open");
        assert_eq!(document.permissions_mode & 0o777, 0o755);

        let save =
            SaveTextRequest::new("script.sh", "#!/bin/sh\necho two\n", document.content_hash);
        service.save_text(&save).expect("save");

        let reopened = service.open_text("script.sh").expect("reopen");
        assert_eq!(reopened.content, "#!/bin/sh\necho two\n");
        assert_eq!(reopened.permissions_mode & 0o777, 0o755);
    }

    // --- Error values never contain file contents --------------------------

    #[test]
    fn error_values_do_not_contain_file_contents() {
        const SENTINEL: &str = "SUPER_SENSITIVE_SENTINEL_VALUE_42";
        let ws = TempWorkspace::new("no-content-leak");
        ws.write("data.txt", &format!("line with {SENTINEL}\n"));
        let service = ws.service();
        let document = service.open_text("data.txt").expect("open");

        // Force a conflict error by mutating on-disk content (which contains the
        // sentinel) and saving with the stale baseline.
        ws.write("data.txt", &format!("changed {SENTINEL} again\n"));
        let save = SaveTextRequest::new("data.txt", "edited\n", document.content_hash);
        let error = service.save_text(&save).expect_err("conflict");

        let display = format!("{error}");
        let debug = format!("{error:?}");
        assert!(!display.contains(SENTINEL), "Display leaked file contents");
        assert!(!debug.contains(SENTINEL), "Debug leaked file contents");
        assert_eq!(error.reason_code(), "file_changed_on_disk");
    }

    // --- Watch -------------------------------------------------------------

    #[test]
    fn watch_detects_external_modification() {
        let ws = TempWorkspace::new("watch");
        ws.write("watched.txt", "start\n");
        let service = ws.service();
        let handle = service.subscribe_changes("watched.txt").expect("subscribe");
        assert_eq!(handle.poll().expect("poll"), WatchChange::Unchanged);

        ws.write("watched.txt", "changed-externally\n");
        match handle.poll().expect("poll after change") {
            WatchChange::Modified { new_hash, .. } => {
                assert_eq!(new_hash, content_hash(b"changed-externally\n"));
            }
            other => panic!("expected Modified, got {other:?}"),
        }
    }

    #[test]
    fn stat_reports_metadata_without_contents() {
        let ws = TempWorkspace::new("stat");
        ws.write("meta.txt", "abc\n");
        let service = ws.service();
        let entry = service.stat("meta.txt").expect("stat");
        assert_eq!(entry.byte_size, 4);
        assert!(!entry.is_secret_restricted);
        assert!(entry.content_hash.is_empty());
    }

    // ======================================================================
    // PR-044 PART B — FUZZ CORPUS (workspace file ingress)
    //
    // Deterministic, in-process fuzzing of the file-service ingress
    // (`open_text` / `stat` / `save_text` / `lexical_candidate`). The
    // adversarial corpus is hostile path strings — traversal (`..`), absolute,
    // Windows drive/UNC, embedded NUL, URL-encoded dot-dot, deeply nested, and
    // long random — plus a real on-disk canary OUTSIDE the workspace root. For
    // every case the ingress MUST:
    //   1. never panic (typed `Result` always),
    //   2. never resolve to or read a path outside the canonical workspace
    //      root (the outside canary is byte-for-byte intact afterwards),
    //   3. on success only ever return contents of an in-root regular file.
    // ======================================================================

    /// Deterministic xorshift64* PRNG.
    struct Lcg(u64);
    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    /// Fixed seed list of adversarial workspace-relative path strings.
    fn adversarial_paths() -> Vec<String> {
        let mut v: Vec<String> = vec![
            "".into(),
            ".".into(),
            "..".into(),
            "../".into(),
            "../etc/passwd".into(),
            "../../../../../../etc/passwd".into(),
            "a/../../b".into(),
            "a/./b/../c".into(),
            "/etc/passwd".into(),
            "//etc/passwd".into(),
            "/".into(),
            "C:/windows".into(),
            "C:\\windows\\system32".into(),
            "\\\\unc\\share\\x".into(),
            "%2e%2e/etc/passwd".into(),
            "%2e%2e%2f%2e%2e%2fetc".into(),
            "name\0with\0nul".into(),
            ".env".into(),
            "secret.key".into(),
            "id_rsa".into(),
            "cred/cred.pem".into(),
            "ok/dir/file.txt".into(),
            "\u{202e}rtl".into(),
            "con".into(),
            "nested/".to_string() + &"deep/".repeat(64) + "leaf.txt",
            "x".repeat(5000),
        ];
        // A handful of segment-fuzzed variants.
        let mut rng = Lcg::new(0x1357_9BDF);
        const SEGS: &[&str] = &["..", ".", "a", "%2e%2e", "\0", "sub", "/", "\\", ".env"];
        for _ in 0..40 {
            let n = (rng.next_u64() % 6) as usize;
            let mut parts = Vec::new();
            for _ in 0..n {
                parts.push(SEGS[(rng.next_u64() as usize) % SEGS.len()]);
            }
            v.push(parts.join("/"));
        }
        v
    }

    #[test]
    fn fuzz_open_text_never_escapes_workspace() {
        let ws = TempWorkspace::new("fuzz-open");
        // A real, in-root regular file the parser is allowed to read.
        ws.write("ok/dir/file.txt", "in-root-ok\n");

        // The OUTSIDE canary lives next to (but not under) the workspace root.
        let mut outside = std::env::temp_dir();
        outside.push(format!(
            "opensks-file-fuzz-OUTSIDE-{}-{}",
            std::process::id(),
            unique()
        ));
        const CANARY: &str = "OUTSIDE-SECRET-DO-NOT-READ";
        fs::write(&outside, CANARY).expect("outside canary");

        let service = ws.service();
        let mut cases = 0usize;
        // Each adversarial path, plus deterministic byte-mutated variants.
        let mut rng = Lcg::new(0x2468_ACE0);
        for base in adversarial_paths() {
            for _ in 0..16 {
                let mut raw = base.clone().into_bytes();
                // mutate a couple of bytes
                if !raw.is_empty() {
                    for _ in 0..((rng.next_u64() % 3) + 1) {
                        let i = (rng.next_u64() as usize) % raw.len();
                        raw[i] = (rng.next_u64() >> 24) as u8;
                    }
                }
                let rel = String::from_utf8_lossy(&raw).to_string();
                cases += 1;
                match service.open_text(&rel) {
                    Ok(doc) => {
                        // A success must NEVER be the outside canary's contents.
                        assert!(
                            !doc.content.contains(CANARY),
                            "open_text escaped the workspace and read the canary via {rel:?}"
                        );
                    }
                    Err(_) => { /* typed rejection is the expected path */ }
                }
                // stat and save_text must be equally contained / panic-free.
                let _ = service.stat(&rel);
                let req = SaveTextRequest::new(&rel, "x", content_hash(b"x"));
                let _ = service.save_text(&req);
                // INVARIANT: the outside canary is never modified or read-through.
                assert_eq!(
                    fs::read_to_string(&outside).expect("canary intact"),
                    CANARY,
                    "the outside canary was touched via {rel:?}"
                );
            }
        }
        let _ = fs::remove_file(&outside);
        assert!(cases >= 500, "fuzzed {cases} file-ingress cases");
    }

    #[test]
    fn fuzz_file_bytes_open_text_never_panics() {
        // Adversarial FILE CONTENTS (not paths): NUL-laden, non-UTF8, huge,
        // truncated multibyte. `open_text` must classify each via a typed error
        // (binary / encoding / too_large) or return a valid UTF-8 document —
        // never panic.
        let ws = TempWorkspace::new("fuzz-bytes");
        let service = WorkspaceFileService::open_with_limit(&ws.root, 1024).expect("svc");
        let mut rng = Lcg::new(0x0BAD_F00D);
        let seeds: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x00],
            vec![0xff, 0xfe, 0xfd],
            b"valid utf8 text\n".to_vec(),
            vec![0xc3, 0x28],       // invalid 2-byte sequence
            vec![0xe2, 0x82],       // truncated 3-byte sequence
            vec![0xf0, 0x9f, 0x98], // truncated 4-byte emoji
            vec![b'a'; 2048],       // over the 1 KiB limit
            (0u8..=255).collect(),
        ];
        let mut cases = 0usize;
        for seed in seeds {
            for _ in 0..40 {
                let mut bytes = seed.clone();
                if !bytes.is_empty() {
                    let i = (rng.next_u64() as usize) % bytes.len();
                    bytes[i] = (rng.next_u64() >> 20) as u8;
                }
                fs::write(ws.path("fuzz.txt"), &bytes).expect("write fuzz file");
                cases += 1;
                match service.open_text("fuzz.txt") {
                    Ok(doc) => {
                        // A returned document is always valid UTF-8 and NUL-free
                        // within the sniff window.
                        assert!(
                            !doc.content.as_bytes()[..doc.content.len().min(BINARY_SNIFF_BYTES)]
                                .contains(&0)
                        );
                    }
                    Err(_) => { /* typed binary/encoding/size rejection */ }
                }
            }
        }
        assert!(cases >= 300, "fuzzed {cases} file-content cases");
    }
}
