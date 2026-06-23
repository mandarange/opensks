//! Design-package import quarantine pipeline (PR-039).
//!
//! This module ingests an **untrusted** design package — either a local
//! directory or a local `.zip` archive — and stages it in an isolated
//! quarantine directory under `.opensks/design-cache/quarantine/<id>/`. It does
//! **not** promote anything: quarantine validates and stores, and a separate,
//! explicit human-review step ([`approve_import`]) re-validates and promotes the
//! package into the registry. Nothing here ever touches the network.
//!
//! ## Quarantine directory layout
//!
//! ```text
//! .opensks/design-cache/quarantine/<quarantine_id>/
//!   payload/              # the extracted / copied package tree (data only)
//!     manifest.json
//!     tokens.json
//!     DESIGN.md
//!     ...
//!   quarantine.json       # the persisted [`QuarantineEntry`] (status + provenance)
//! ```
//!
//! The `payload/` subtree is the *only* place bytes are written; every archive
//! entry and every copied file is resolved against `payload/` and rejected if it
//! would escape, is a symlink, is executable/script, or busts a size/count cap.
//!
//! ## Defenses (each maps to a stable [`RejectedReason`])
//!
//! - **zip-slip** — an archive entry whose normalized path escapes `payload/`
//!   (`..`, absolute, drive/UNC prefix) → [`RejectedReason::ZipSlip`]. The file
//!   is never written outside the quarantine dir.
//! - **symlink** — an archive entry flagged as a symlink, or a copied
//!   source file/dir that is a symlink (checked with `symlink_metadata`, never
//!   followed) → [`RejectedReason::Symlink`].
//! - **executable/script** — a file with a unix executable bit, a
//!   script/binary extension (`.sh`, `.command`, `.rb`, `.py`, `.js`, `.bin`,
//!   …), a `#!` shebang, or Mach-O/ELF magic → [`RejectedReason::ExecutableOrScript`].
//! - **limits** — too many files ([`RejectedReason::TooManyFiles`]), too many
//!   total bytes ([`RejectedReason::TooLarge`]), too many archive entries
//!   ([`RejectedReason::TooManyArchiveEntries`]), or a single entry whose
//!   uncompressed size exceeds the per-entry cap (zip-bomb guard, also
//!   [`RejectedReason::TooLarge`]).
//! - **MIME allowlist** — a file whose extension is not an allowed design
//!   data type (`json`, `md`/`txt`/`text`, `png`/`jpg`/`jpeg`/`svg`) →
//!   [`RejectedReason::MimeNotAllowed`].
//!
//! A rejection deletes the partially-staged payload and records a `rejected`
//! entry: there is never a partial promotion.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use opensks_contracts::DesignPackageManifest;

use crate::registry::{
    DesignRegistry, DesignRegistryError, MANIFEST_FILE_NAME, is_valid_package_id, validate_package,
};

/// Subdirectory under `.opensks/design-cache/` that holds all quarantines.
pub const QUARANTINE_DIR: &str = "quarantine";
/// The extracted/copied package tree inside a quarantine dir.
pub const PAYLOAD_DIR: &str = "payload";
/// The persisted quarantine-entry sidecar file name.
pub const ENTRY_FILE_NAME: &str = "quarantine.json";
/// Where promoted packages land (under the workspace).
pub const DESIGN_SYSTEMS_DIR: &str = "design-systems";

/// Default strict ingestion limits. Conservative on purpose: a design package is
/// small, data-only content.
pub const DEFAULT_MAX_FILE_COUNT: usize = 512;
/// Default cap on the total uncompressed byte size across all files.
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
/// Default cap on the number of central-directory entries in an archive.
pub const DEFAULT_MAX_ARCHIVE_ENTRIES: usize = 1024;
/// Default per-entry uncompressed-size cap (zip-bomb guard).
pub const DEFAULT_MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;

/// Whether an import came from a local directory or a local `.zip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    /// A local directory tree.
    Local,
    /// A local `.zip` archive.
    Archive,
}

impl ImportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Archive => "archive",
        }
    }

    /// Parse the `--kind` flag value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "local" => Some(Self::Local),
            "archive" => Some(Self::Archive),
            _ => None,
        }
    }
}

/// Final disposition of a quarantined import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineStatus {
    /// Staged and passed all ingestion defenses; awaiting human-review approval.
    Quarantined,
    /// Failed an ingestion defense; staged bytes were removed.
    Rejected,
}

impl QuarantineStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quarantined => "quarantined",
            Self::Rejected => "rejected",
        }
    }
}

/// Stable machine-readable reason an import was rejected. Mirrors the shared
/// import JSON contract's `rejected_reason` values exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectedReason {
    ZipSlip,
    Symlink,
    ExecutableOrScript,
    TooManyFiles,
    TooLarge,
    MimeNotAllowed,
    TooManyArchiveEntries,
}

impl RejectedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ZipSlip => "zip_slip",
            Self::Symlink => "symlink",
            Self::ExecutableOrScript => "executable_or_script",
            Self::TooManyFiles => "too_many_files",
            Self::TooLarge => "too_large",
            Self::MimeNotAllowed => "mime_not_allowed",
            Self::TooManyArchiveEntries => "too_many_archive_entries",
        }
    }
}

/// Recorded origin of an import, redacted of any secret/credential material.
/// `license` is read from the package manifest when present, else `None`;
/// `commit` is always `None` for local imports (no VCS resolution, no network).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// A human-meaningful, non-secret source reference (the basename of the
    /// provided source path).
    pub source: String,
    pub license: Option<String>,
    pub commit: Option<String>,
}

/// A persisted quarantine entry: identity, disposition, counts, and provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineEntry {
    pub quarantine_id: String,
    pub status: QuarantineStatus,
    pub kind: ImportKind,
    pub provenance: Provenance,
    pub file_count: u64,
    pub byte_size: u64,
    pub rejected_reason: Option<RejectedReason>,
}

impl QuarantineEntry {
    /// Serialize to the stable on-disk `quarantine.json` shape (snake_case).
    pub fn to_json(&self) -> String {
        let license = match &self.provenance.license {
            Some(value) => format!("\"{}\"", json_escape(value)),
            None => "null".to_string(),
        };
        let commit = match &self.provenance.commit {
            Some(value) => format!("\"{}\"", json_escape(value)),
            None => "null".to_string(),
        };
        let reason = match self.rejected_reason {
            Some(reason) => format!("\"{}\"", reason.as_str()),
            None => "null".to_string(),
        };
        format!(
            "{{\"schema\":\"opensks.design-quarantine-entry.v1\",\
\"quarantine_id\":\"{id}\",\
\"status\":\"{status}\",\
\"kind\":\"{kind}\",\
\"provenance\":{{\"source\":\"{source}\",\"license\":{license},\"commit\":{commit}}},\
\"file_count\":{file_count},\
\"byte_size\":{byte_size},\
\"rejected_reason\":{reason}}}",
            id = json_escape(&self.quarantine_id),
            status = self.status.as_str(),
            kind = self.kind.as_str(),
            source = json_escape(&self.provenance.source),
            file_count = self.file_count,
            byte_size = self.byte_size,
        )
    }

    /// Parse a persisted `quarantine.json`. Tolerant of field order; rejects on
    /// missing required fields.
    pub fn from_json(text: &str) -> Result<Self, ImportError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|_| ImportError::EntryUnreadable)?;
        let obj = value.as_object().ok_or(ImportError::EntryUnreadable)?;
        let get_str = |key: &str| -> Result<String, ImportError> {
            obj.get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or(ImportError::EntryUnreadable)
        };
        let quarantine_id = get_str("quarantine_id")?;
        let status = match get_str("status")?.as_str() {
            "quarantined" => QuarantineStatus::Quarantined,
            "rejected" => QuarantineStatus::Rejected,
            _ => return Err(ImportError::EntryUnreadable),
        };
        let kind = match get_str("kind")?.as_str() {
            "local" => ImportKind::Local,
            "archive" => ImportKind::Archive,
            _ => return Err(ImportError::EntryUnreadable),
        };
        let provenance_obj = obj
            .get("provenance")
            .and_then(|v| v.as_object())
            .ok_or(ImportError::EntryUnreadable)?;
        let source = provenance_obj
            .get("source")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or(ImportError::EntryUnreadable)?;
        let license = provenance_obj
            .get("license")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let commit = provenance_obj
            .get("commit")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let file_count = obj.get("file_count").and_then(|v| v.as_u64()).unwrap_or(0);
        let byte_size = obj.get("byte_size").and_then(|v| v.as_u64()).unwrap_or(0);
        let rejected_reason = match obj.get("rejected_reason").and_then(|v| v.as_str()) {
            Some("zip_slip") => Some(RejectedReason::ZipSlip),
            Some("symlink") => Some(RejectedReason::Symlink),
            Some("executable_or_script") => Some(RejectedReason::ExecutableOrScript),
            Some("too_many_files") => Some(RejectedReason::TooManyFiles),
            Some("too_large") => Some(RejectedReason::TooLarge),
            Some("mime_not_allowed") => Some(RejectedReason::MimeNotAllowed),
            Some("too_many_archive_entries") => Some(RejectedReason::TooManyArchiveEntries),
            _ => None,
        };
        Ok(Self {
            quarantine_id,
            status,
            kind,
            provenance: Provenance {
                source,
                license,
                commit,
            },
            file_count,
            byte_size,
            rejected_reason,
        })
    }
}

/// Tunable ingestion limits. Use [`ImportLimits::default`] for the strict
/// production caps.
#[derive(Debug, Clone, Copy)]
pub struct ImportLimits {
    pub max_file_count: usize,
    pub max_total_bytes: u64,
    pub max_archive_entries: usize,
    pub max_entry_bytes: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_file_count: DEFAULT_MAX_FILE_COUNT,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_archive_entries: DEFAULT_MAX_ARCHIVE_ENTRIES,
            max_entry_bytes: DEFAULT_MAX_ENTRY_BYTES,
        }
    }
}

/// Hard error taxonomy for the import pipeline. These are *operator* errors
/// (missing source, IO failure, malformed entry) — distinct from a security
/// *rejection*, which is a successful run that records `status: rejected`.
#[derive(Debug)]
pub enum ImportError {
    /// `--source` does not exist or is the wrong type for `--kind`.
    SourceMissing { source: String },
    /// `--kind archive` was given a path that is not a readable `.zip`.
    NotAnArchive { source: String },
    /// A quarantine id was requested that does not exist on disk.
    QuarantineNotFound { quarantine_id: String },
    /// A persisted `quarantine.json` could not be parsed.
    EntryUnreadable,
    /// Promotion target id collides with an existing registry package.
    AlreadyPromoted { package_id: String },
    /// Re-validation at approval time failed (the quarantined bytes do not pass
    /// the PR-037 registry validation).
    Validation(DesignRegistryError),
    /// An underlying filesystem error.
    Io(io::Error),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceMissing { source } => {
                write!(f, "design_import_source_missing: {source}")
            }
            Self::NotAnArchive { source } => write!(f, "design_import_not_an_archive: {source}"),
            Self::QuarantineNotFound { quarantine_id } => {
                write!(f, "design_import_quarantine_not_found: {quarantine_id}")
            }
            Self::EntryUnreadable => write!(f, "design_import_entry_unreadable"),
            Self::AlreadyPromoted { package_id } => {
                write!(f, "design_import_already_promoted: {package_id}")
            }
            Self::Validation(error) => write!(f, "design_import_validation: {error}"),
            Self::Io(error) => write!(f, "design_import_io: {error}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<io::Error> for ImportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Outcome of [`quarantine_import`]: the persisted entry plus its quarantine dir.
#[derive(Debug, Clone)]
pub struct ImportOutcome {
    pub entry: QuarantineEntry,
    pub quarantine_dir: PathBuf,
}

/// Outcome of [`approve_import`].
#[derive(Debug, Clone)]
pub struct ApproveOutcome {
    pub package_id: String,
    pub promoted: bool,
}

/// The `.opensks/design-cache/quarantine` root under a workspace.
pub fn quarantine_root(workspace: &Path) -> PathBuf {
    workspace
        .join(".opensks")
        .join("design-cache")
        .join(QUARANTINE_DIR)
}

/// The `.opensks/design-systems` registry root under a workspace.
pub fn design_systems_root(workspace: &Path) -> PathBuf {
    workspace.join(".opensks").join(DESIGN_SYSTEMS_DIR)
}

/// Quarantine an untrusted import. Extracts/copies the `source` into a fresh
/// per-import quarantine dir, applies every ingestion defense, and persists a
/// [`QuarantineEntry`]. A defense failure yields `Ok` with a `rejected` entry
/// (staged bytes removed); only operator errors (missing source, IO) return
/// `Err`. **Never** promotes and **never** touches the network.
pub fn quarantine_import(
    workspace: &Path,
    source: &Path,
    kind: ImportKind,
    limits: &ImportLimits,
) -> Result<ImportOutcome, ImportError> {
    // Validate the source up front (operator error, not a security rejection).
    let source_meta = fs::symlink_metadata(source).map_err(|_| ImportError::SourceMissing {
        source: source.display().to_string(),
    })?;
    match kind {
        ImportKind::Local => {
            if !source_meta.is_dir() {
                return Err(ImportError::SourceMissing {
                    source: source.display().to_string(),
                });
            }
        }
        ImportKind::Archive => {
            if !source_meta.is_file() {
                return Err(ImportError::NotAnArchive {
                    source: source.display().to_string(),
                });
            }
        }
    }

    let root = quarantine_root(workspace);
    fs::create_dir_all(&root)?;
    let quarantine_id = mint_quarantine_id();
    let quarantine_dir = root.join(&quarantine_id);
    // Fresh, non-colliding dir. Canonicalize the root so containment checks are
    // robust to symlinked temp roots.
    let canonical_root = root
        .canonicalize()
        .unwrap_or_else(|_| root.clone())
        .join(&quarantine_id);
    fs::create_dir_all(&quarantine_dir)?;
    let payload_dir = quarantine_dir.join(PAYLOAD_DIR);
    fs::create_dir_all(&payload_dir)?;
    let canonical_payload = canonical_root.join(PAYLOAD_DIR);

    let source_label = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("import")
        .to_string();

    // Stage the payload, enforcing every defense.
    let staged = match kind {
        ImportKind::Local => stage_directory(source, &payload_dir, &canonical_payload, limits),
        ImportKind::Archive => stage_archive(source, &payload_dir, &canonical_payload, limits),
    };

    match staged {
        Ok(stats) => {
            // Read license from the package manifest if present (no network).
            let license = read_manifest_license(&payload_dir);
            let entry = QuarantineEntry {
                quarantine_id,
                status: QuarantineStatus::Quarantined,
                kind,
                provenance: Provenance {
                    source: source_label,
                    license,
                    commit: None,
                },
                file_count: stats.file_count,
                byte_size: stats.byte_size,
                rejected_reason: None,
            };
            persist_entry(&quarantine_dir, &entry)?;
            Ok(ImportOutcome {
                entry,
                quarantine_dir,
            })
        }
        Err(StageError::Rejected(reason)) => {
            // No partial promotion: remove the staged payload bytes; keep only
            // the rejection record under the quarantine dir.
            let _ = remove_dir_within(&payload_dir, &canonical_root);
            let entry = QuarantineEntry {
                quarantine_id,
                status: QuarantineStatus::Rejected,
                kind,
                provenance: Provenance {
                    source: source_label,
                    license: None,
                    commit: None,
                },
                file_count: 0,
                byte_size: 0,
                rejected_reason: Some(reason),
            };
            persist_entry(&quarantine_dir, &entry)?;
            Ok(ImportOutcome {
                entry,
                quarantine_dir,
            })
        }
        Err(StageError::Io(error)) => Err(ImportError::Io(error)),
    }
}

/// Promote a previously-quarantined package into the registry — the *only* path
/// that writes into `.opensks/design-systems/`. RE-runs every ingestion defense
/// over the staged payload, then runs the PR-037 [`validate_package`] before the
/// directory is copied. This is the human-review gate.
pub fn approve_import(
    workspace: &Path,
    quarantine_id: &str,
) -> Result<ApproveOutcome, ImportError> {
    let quarantine_dir = locate_quarantine(workspace, quarantine_id)?;
    let payload_dir = quarantine_dir.join(PAYLOAD_DIR);
    let canonical_quarantine =
        quarantine_dir
            .canonicalize()
            .map_err(|_| ImportError::QuarantineNotFound {
                quarantine_id: quarantine_id.to_string(),
            })?;
    let canonical_payload = canonical_quarantine.join(PAYLOAD_DIR);

    // RE-validate the staged tree with the full ingestion defense set. This
    // re-checks symlinks, executables, MIME, and size/count limits over what is
    // on disk now (TOCTOU-resistant: approval trusts disk, not the import-time
    // record).
    let limits = ImportLimits::default();
    rescan_payload(&payload_dir, &canonical_payload, &limits)
        .map_err(stage_error_to_import_error)?;

    // The manifest's id determines the package directory; it must be a single
    // safe id segment.
    let manifest = read_manifest(&payload_dir)?;
    let package_id = manifest.id.clone();
    if !is_valid_package_id(&package_id) {
        return Err(ImportError::Validation(
            DesignRegistryError::InvalidPackageId {
                id: package_id.clone(),
            },
        ));
    }

    // PR-037 strict registry validation over the staged payload (paths, hashes,
    // license, symlinks). The payload dir is named by its package id only for
    // validation purposes; validate_package compares manifest.id to the dir id,
    // so validate against a transiently-named view.
    validate_staged_as_package(&payload_dir, &package_id).map_err(ImportError::Validation)?;

    // Refuse to clobber an already-promoted package of the same id.
    let target = design_systems_root(workspace).join(&package_id);
    if target.join(MANIFEST_FILE_NAME).exists() {
        return Err(ImportError::AlreadyPromoted { package_id });
    }

    // Promote: copy the validated payload into the registry, then re-validate in
    // place via the registry to prove it resolves.
    fs::create_dir_all(design_systems_root(workspace))?;
    copy_tree(&payload_dir, &target)?;
    let registry = DesignRegistry::with_default_order(workspace, None);
    if let Err(error) = registry.resolve(&package_id) {
        // Roll back a promotion that does not resolve so the registry is never
        // left with a half-written package.
        let canonical_systems = design_systems_root(workspace)
            .canonicalize()
            .unwrap_or_else(|_| design_systems_root(workspace));
        let _ = remove_dir_within(&target, &canonical_systems);
        return Err(ImportError::Validation(error));
    }

    Ok(ApproveOutcome {
        package_id,
        promoted: true,
    })
}

/// Safely delete a quarantine dir. Canonicalizes the target and confirms it is
/// inside the quarantine root before `remove_dir_all`, so a crafted id can never
/// delete outside the quarantine root.
pub fn reject_import(workspace: &Path, quarantine_id: &str) -> Result<bool, ImportError> {
    let quarantine_dir = locate_quarantine(workspace, quarantine_id)?;
    let root = quarantine_root(workspace);
    let canonical_root = root
        .canonicalize()
        .map_err(|_| ImportError::QuarantineNotFound {
            quarantine_id: quarantine_id.to_string(),
        })?;
    remove_dir_within(&quarantine_dir, &canonical_root)?;
    Ok(true)
}

/// List quarantined entries on disk, newest-id first by directory name order.
pub fn list_quarantines(workspace: &Path) -> Result<Vec<QuarantineEntry>, ImportError> {
    let root = quarantine_root(workspace);
    let mut entries = Vec::new();
    let read = match fs::read_dir(&root) {
        Ok(read) => read,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => return Err(ImportError::Io(error)),
    };
    let mut dirs: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let entry_path = dir.join(ENTRY_FILE_NAME);
        let Ok(text) = fs::read_to_string(&entry_path) else {
            continue;
        };
        if let Ok(entry) = QuarantineEntry::from_json(&text) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

// ---- internals -----------------------------------------------------------

/// Per-import accumulated stats.
struct StageStats {
    file_count: u64,
    byte_size: u64,
}

/// A staging step either succeeds, hits a security rejection, or hits an IO
/// error. A rejection is *not* an IO error: it is recorded, not propagated.
enum StageError {
    Rejected(RejectedReason),
    Io(io::Error),
}

impl From<io::Error> for StageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn stage_error_to_import_error(error: StageError) -> ImportError {
    match error {
        StageError::Rejected(reason) => {
            // At approval time a defense failure is a hard refusal (the package
            // should never have been quarantined as clean). Surface it as a
            // validation error carrying the reason code.
            ImportError::Validation(DesignRegistryError::PathEscape {
                id: "quarantine".to_string(),
                path: reason.as_str().to_string(),
            })
        }
        StageError::Io(error) => ImportError::Io(error),
    }
}

/// Mint a fresh, monotonic-ish quarantine id from the clock + pid. Safe as a
/// single path segment.
fn mint_quarantine_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("q-{}-{nanos}", std::process::id())
}

/// Copy a local directory tree into `payload_dir`, enforcing all defenses.
fn stage_directory(
    source: &Path,
    payload_dir: &Path,
    canonical_payload: &Path,
    limits: &ImportLimits,
) -> Result<StageStats, StageError> {
    let mut stats = StageStats {
        file_count: 0,
        byte_size: 0,
    };
    copy_dir_guarded(
        source,
        Path::new(""),
        payload_dir,
        canonical_payload,
        limits,
        &mut stats,
    )?;
    Ok(stats)
}

/// Recursively copy `source` into `payload_dir`, rejecting symlinks, executables,
/// disallowed MIME, and over-limit counts/sizes. `rel` is the path relative to
/// the payload root, used for containment checks.
fn copy_dir_guarded(
    source: &Path,
    rel: &Path,
    payload_dir: &Path,
    canonical_payload: &Path,
    limits: &ImportLimits,
    stats: &mut StageStats,
) -> Result<(), StageError> {
    let mut entries: Vec<PathBuf> = fs::read_dir(source)?.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => return Err(StageError::Rejected(RejectedReason::MimeNotAllowed)),
        };
        let child_rel = rel.join(&name);
        // Never follow links: a symlinked entry (file OR dir) is rejected.
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(StageError::Rejected(RejectedReason::Symlink));
        }
        // Containment: the destination must stay inside the payload dir.
        let dest = resolve_within(payload_dir, canonical_payload, &child_rel)
            .ok_or(StageError::Rejected(RejectedReason::ZipSlip))?;
        if meta.is_dir() {
            fs::create_dir_all(&dest)?;
            copy_dir_guarded(
                &path,
                &child_rel,
                payload_dir,
                canonical_payload,
                limits,
                stats,
            )?;
        } else if meta.is_file() {
            let bytes = fs::read(&path)?;
            let executable_bit = unix_is_executable(&meta);
            stage_file_bytes(&child_rel, &dest, &bytes, executable_bit, limits, stats)?;
        } else {
            // Sockets, fifos, devices: not design data.
            return Err(StageError::Rejected(RejectedReason::ExecutableOrScript));
        }
    }
    Ok(())
}

/// Validate a single file's bytes against MIME/script/exec/size defenses, then
/// write it to `dest` and update the running stats. Bumps and checks the file
/// count and total byte caps.
fn stage_file_bytes(
    rel: &Path,
    dest: &Path,
    bytes: &[u8],
    executable_bit: bool,
    limits: &ImportLimits,
    stats: &mut StageStats,
) -> Result<(), StageError> {
    let rel_str = rel.to_string_lossy();
    // Per-entry size cap (zip-bomb guard) is checked before write.
    if bytes.len() as u64 > limits.max_entry_bytes {
        return Err(StageError::Rejected(RejectedReason::TooLarge));
    }
    if executable_bit {
        return Err(StageError::Rejected(RejectedReason::ExecutableOrScript));
    }
    if is_script_or_binary(&rel_str, bytes) {
        return Err(StageError::Rejected(RejectedReason::ExecutableOrScript));
    }
    if !is_allowed_mime(&rel_str) {
        return Err(StageError::Rejected(RejectedReason::MimeNotAllowed));
    }

    let next_count = stats.file_count + 1;
    if next_count as usize > limits.max_file_count {
        return Err(StageError::Rejected(RejectedReason::TooManyFiles));
    }
    let next_bytes = stats.byte_size + bytes.len() as u64;
    if next_bytes > limits.max_total_bytes {
        return Err(StageError::Rejected(RejectedReason::TooLarge));
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest, bytes)?;
    stats.file_count = next_count;
    stats.byte_size = next_bytes;
    Ok(())
}

/// Extract a local `.zip` into `payload_dir`, enforcing all defenses including
/// the archive-entry cap and zip-slip on the *declared* entry name (so the
/// escaping file is never written).
fn stage_archive(
    source: &Path,
    payload_dir: &Path,
    canonical_payload: &Path,
    limits: &ImportLimits,
) -> Result<StageStats, StageError> {
    let archive_bytes = fs::read(source)?;
    let entries = zip::read_central_directory(&archive_bytes)
        .map_err(|_| StageError::Rejected(RejectedReason::MimeNotAllowed))?;
    if entries.len() > limits.max_archive_entries {
        return Err(StageError::Rejected(RejectedReason::TooManyArchiveEntries));
    }
    let mut stats = StageStats {
        file_count: 0,
        byte_size: 0,
    };
    for entry in &entries {
        // Directory entries (trailing slash) are created lazily by their files;
        // skip explicit dir records but still reject if they try to escape.
        if entry.is_dir {
            // A zip-slip on a directory name must still be caught.
            if normalize_archive_path(&entry.name).is_none() {
                return Err(StageError::Rejected(RejectedReason::ZipSlip));
            }
            continue;
        }
        // A zip entry flagged as a symlink (unix mode S_IFLNK) is rejected
        // before any byte is written.
        if entry.is_symlink {
            return Err(StageError::Rejected(RejectedReason::Symlink));
        }
        // Per-entry uncompressed-size cap from the central directory (zip-bomb
        // guard) — checked before we even inflate.
        if entry.uncompressed_size > limits.max_entry_bytes {
            return Err(StageError::Rejected(RejectedReason::TooLarge));
        }
        // zip-slip: reject an entry whose normalized path escapes payload.
        let normalized = normalize_archive_path(&entry.name)
            .ok_or(StageError::Rejected(RejectedReason::ZipSlip))?;
        let dest = resolve_within(payload_dir, canonical_payload, &normalized)
            .ok_or(StageError::Rejected(RejectedReason::ZipSlip))?;

        let data = zip::extract_entry(&archive_bytes, entry)
            .map_err(|_| StageError::Rejected(RejectedReason::MimeNotAllowed))?;
        // Re-check inflated size against the per-entry cap (defends against a
        // lying central directory).
        if data.len() as u64 > limits.max_entry_bytes {
            return Err(StageError::Rejected(RejectedReason::TooLarge));
        }
        let executable_bit = entry.is_executable;
        stage_file_bytes(
            &normalized,
            &dest,
            &data,
            executable_bit,
            limits,
            &mut stats,
        )?;
    }
    Ok(stats)
}

/// Re-scan an already-staged payload tree at approval time, applying the same
/// defenses without re-writing anything.
fn rescan_payload(
    payload_dir: &Path,
    canonical_payload: &Path,
    limits: &ImportLimits,
) -> Result<(), StageError> {
    let mut stats = StageStats {
        file_count: 0,
        byte_size: 0,
    };
    rescan_dir(
        payload_dir,
        Path::new(""),
        canonical_payload,
        limits,
        &mut stats,
    )
}

fn rescan_dir(
    dir: &Path,
    rel: &Path,
    canonical_payload: &Path,
    limits: &ImportLimits,
    stats: &mut StageStats,
) -> Result<(), StageError> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => return Err(StageError::Rejected(RejectedReason::MimeNotAllowed)),
        };
        let child_rel = rel.join(&name);
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(StageError::Rejected(RejectedReason::Symlink));
        }
        // Containment of the existing path under payload.
        if resolve_within(dir_root(dir, rel), canonical_payload, &child_rel).is_none() {
            return Err(StageError::Rejected(RejectedReason::ZipSlip));
        }
        if meta.is_dir() {
            rescan_dir(&path, &child_rel, canonical_payload, limits, stats)?;
        } else if meta.is_file() {
            let bytes = fs::read(&path)?;
            let rel_str = child_rel.to_string_lossy();
            if bytes.len() as u64 > limits.max_entry_bytes {
                return Err(StageError::Rejected(RejectedReason::TooLarge));
            }
            if unix_is_executable(&meta) || is_script_or_binary(&rel_str, &bytes) {
                return Err(StageError::Rejected(RejectedReason::ExecutableOrScript));
            }
            if !is_allowed_mime(&rel_str) {
                return Err(StageError::Rejected(RejectedReason::MimeNotAllowed));
            }
            stats.file_count += 1;
            if stats.file_count as usize > limits.max_file_count {
                return Err(StageError::Rejected(RejectedReason::TooManyFiles));
            }
            stats.byte_size += bytes.len() as u64;
            if stats.byte_size > limits.max_total_bytes {
                return Err(StageError::Rejected(RejectedReason::TooLarge));
            }
        } else {
            return Err(StageError::Rejected(RejectedReason::ExecutableOrScript));
        }
    }
    Ok(())
}

/// Compute the payload-root path that `rel` was relative to (the rescan starts
/// at `payload_dir` with an empty `rel`, so the root is always `payload_dir`).
fn dir_root<'a>(payload_dir: &'a Path, _rel: &Path) -> &'a Path {
    payload_dir
}

/// Resolve `rel` under `base`, returning the concrete destination path only if
/// it canonically stays within `canonical_base`. `rel` is already normalized
/// (no `..`, no absolute components) by the caller, but we re-verify against the
/// canonical base to defend against symlinked ancestors.
fn resolve_within(base: &Path, canonical_base: &Path, rel: &Path) -> Option<PathBuf> {
    // Reject any non-Normal component defensively.
    let mut normalized = PathBuf::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let candidate = base.join(&normalized);
    // The deepest existing ancestor must canonicalize inside canonical_base.
    let existing = deepest_existing_ancestor(&candidate);
    match existing.canonicalize() {
        Ok(canonical_existing) => {
            if canonical_existing.starts_with(canonical_base) {
                Some(candidate)
            } else {
                None
            }
        }
        // If even the base does not yet canonicalize, fall back to the lexical
        // containment we already proved by rejecting `..`/abs above.
        Err(_) => Some(candidate),
    }
}

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

/// Normalize an archive entry path to a payload-relative path, or `None` if it
/// escapes (absolute, drive/UNC prefix, or a `..` that would climb above root).
fn normalize_archive_path(name: &str) -> Option<PathBuf> {
    // Reject backslash-bearing absolute/UNC Windows paths explicitly: treat `\`
    // as a separator for the purpose of escape detection.
    let unified = name.replace('\\', "/");
    if unified.starts_with('/') {
        return None;
    }
    // A Windows drive prefix like `C:`.
    if unified.len() >= 2 && unified.as_bytes()[1] == b':' {
        return None;
    }
    let mut normalized = PathBuf::new();
    for segment in unified.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            other => normalized.push(other),
        }
    }
    if normalized.as_os_str().is_empty() {
        return None;
    }
    Some(normalized)
}

/// True if a unix file mode carries any executable bit.
#[cfg(unix)]
fn unix_is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn unix_is_executable(_meta: &fs::Metadata) -> bool {
    false
}

/// True if the file looks like a script or native binary by extension, shebang,
/// or magic bytes. A design package is data only, so any of these is rejected.
fn is_script_or_binary(rel: &str, bytes: &[u8]) -> bool {
    let lower = rel.to_ascii_lowercase();
    const SCRIPT_EXTS: &[&str] = &[
        ".sh",
        ".bash",
        ".zsh",
        ".command",
        ".rb",
        ".py",
        ".pyc",
        ".js",
        ".mjs",
        ".cjs",
        ".pl",
        ".php",
        ".bin",
        ".exe",
        ".dll",
        ".dylib",
        ".so",
        ".o",
        ".a",
        ".class",
        ".jar",
        ".bat",
        ".cmd",
        ".ps1",
        ".com",
        ".scpt",
        ".applescript",
        ".vbs",
        ".wasm",
    ];
    if SCRIPT_EXTS.iter().any(|ext| lower.ends_with(ext)) {
        return true;
    }
    // Shebang.
    if bytes.starts_with(b"#!") {
        return true;
    }
    // Native binary magic: ELF, Mach-O (32/64, both endiannesses), PE.
    const MAGICS: &[&[u8]] = &[
        b"\x7fELF",                // ELF
        &[0xFE, 0xED, 0xFA, 0xCE], // Mach-O 32 BE
        &[0xFE, 0xED, 0xFA, 0xCF], // Mach-O 64 BE
        &[0xCE, 0xFA, 0xED, 0xFE], // Mach-O 32 LE
        &[0xCF, 0xFA, 0xED, 0xFE], // Mach-O 64 LE
        &[0xCA, 0xFE, 0xBA, 0xBE], // Mach-O universal / Java class
        b"MZ",                     // PE/DOS
    ];
    MAGICS.iter().any(|magic| bytes.starts_with(magic))
}

/// True if the file extension is on the design-data allowlist.
fn is_allowed_mime(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    const ALLOWED_EXTS: &[&str] = &[
        ".json",
        ".md",
        ".markdown",
        ".txt",
        ".text",
        ".png",
        ".jpg",
        ".jpeg",
        ".svg",
    ];
    ALLOWED_EXTS.iter().any(|ext| lower.ends_with(ext))
}

/// Read the `license` field from a staged `payload/manifest.json` if present.
/// Best-effort and never fails the import; a missing/invalid manifest yields
/// `None` (provenance license is recorded as null).
fn read_manifest_license(payload_dir: &Path) -> Option<String> {
    let manifest_path = payload_dir.join(MANIFEST_FILE_NAME);
    let bytes = fs::read(&manifest_path).ok()?;
    let manifest: DesignPackageManifest = serde_json::from_slice(&bytes).ok()?;
    let license = manifest.license.trim();
    if license.is_empty() {
        None
    } else {
        Some(license.to_string())
    }
}

/// Parse the staged `payload/manifest.json`.
fn read_manifest(payload_dir: &Path) -> Result<DesignPackageManifest, ImportError> {
    let manifest_path = payload_dir.join(MANIFEST_FILE_NAME);
    let bytes = fs::read(&manifest_path).map_err(|_| ImportError::EntryUnreadable)?;
    serde_json::from_slice(&bytes).map_err(|_| ImportError::EntryUnreadable)
}

/// Run PR-037 [`validate_package`] over the staged payload, using a transient
/// symlinked alias named by the package id so `validate_package`'s id/dir check
/// passes. Falls back to validating in-place if the alias cannot be created.
fn validate_staged_as_package(
    payload_dir: &Path,
    package_id: &str,
) -> Result<(), DesignRegistryError> {
    // validate_package compares manifest.id to the directory's final segment, so
    // the payload dir must be addressable under the package id. Create a sibling
    // dir named <package_id> by copying (cheap; packages are tiny) and validate
    // it, then remove it.
    let parent = payload_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let alias = parent.join(package_id);
    // Avoid clobbering if an alias somehow exists.
    if alias.exists() {
        let _ = fs::remove_dir_all(&alias);
    }
    if copy_tree(payload_dir, &alias).is_err() {
        // Last resort: if the payload dir is itself named the package id.
        if payload_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == package_id)
            .unwrap_or(false)
        {
            return validate_package(package_id, payload_dir).map(|_| ());
        }
        return Err(DesignRegistryError::PackageNotFound {
            id: package_id.to_string(),
        });
    }
    let result = validate_package(package_id, &alias).map(|_| ());
    let _ = fs::remove_dir_all(&alias);
    result
}

/// Recursively copy a directory tree (regular files + dirs only; this is invoked
/// only on already-validated, symlink-free payload trees).
fn copy_tree(src: &Path, dest: &Path) -> Result<(), io::Error> {
    fs::create_dir_all(dest)?;
    let mut entries: Vec<PathBuf> = fs::read_dir(src)?.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
        let target = dest.join(name);
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            // Defensive: never copy a symlink (payload trees are symlink-free).
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to copy symlink",
            ));
        }
        if meta.is_dir() {
            copy_tree(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Find a quarantine dir by id, confirming containment within the quarantine
/// root. Rejects an id that is not a single safe path segment.
fn locate_quarantine(workspace: &Path, quarantine_id: &str) -> Result<PathBuf, ImportError> {
    if !is_safe_segment(quarantine_id) {
        return Err(ImportError::QuarantineNotFound {
            quarantine_id: quarantine_id.to_string(),
        });
    }
    let dir = quarantine_root(workspace).join(quarantine_id);
    if !dir.join(ENTRY_FILE_NAME).exists() {
        return Err(ImportError::QuarantineNotFound {
            quarantine_id: quarantine_id.to_string(),
        });
    }
    Ok(dir)
}

/// Persist a quarantine entry sidecar.
fn persist_entry(quarantine_dir: &Path, entry: &QuarantineEntry) -> Result<(), ImportError> {
    let path = quarantine_dir.join(ENTRY_FILE_NAME);
    fs::write(path, entry.to_json())?;
    Ok(())
}

/// Remove `target` only after confirming it canonicalizes inside `canonical_root`.
/// This is the safe-delete guard: a path that escapes the root is never removed.
fn remove_dir_within(target: &Path, canonical_root: &Path) -> Result<(), io::Error> {
    if !target.exists() {
        return Ok(());
    }
    let canonical_target = target.canonicalize()?;
    // The target must be the root itself or strictly inside it.
    if !canonical_target.starts_with(canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to remove a path outside the quarantine root",
        ));
    }
    fs::remove_dir_all(canonical_target)
}

/// A safe single-path-segment id: non-empty, not `.`/`..`, alnum + `-`/`_`.
fn is_safe_segment(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Minimal JSON string escaper for the small set of fields we serialize.
fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Build the import-status JSON (`opensks.design-import-status.v1`) for a list of
/// quarantine entries.
pub fn render_status_json(entries: &[QuarantineEntry]) -> String {
    let items: Vec<String> = entries.iter().map(QuarantineEntry::to_json).collect();
    format!(
        "{{\"schema\":\"opensks.design-import-status.v1\",\"quarantined\":[{}]}}",
        items.join(",")
    )
}

// ---- minimal, dependency-free ZIP central-directory reader ---------------
//
// We deliberately avoid pulling in a third-party `zip` crate to keep the
// untrusted-archive parser small, auditable, and `unsafe`-free. It supports the
// stored (0) and deflate (8) methods; anything else is rejected. It reads the
// End-Of-Central-Directory record, walks the central directory for the
// authoritative entry list (names, sizes, unix mode, method, local-header
// offset), and inflates per-entry data on demand.
mod zip {
    /// A parsed central-directory entry.
    #[derive(Debug, Clone)]
    pub struct ZipEntry {
        pub name: String,
        pub is_dir: bool,
        pub is_symlink: bool,
        pub is_executable: bool,
        pub method: u16,
        pub compressed_size: u64,
        pub uncompressed_size: u64,
        pub local_header_offset: u64,
    }

    #[derive(Debug)]
    pub enum ZipError {
        Malformed,
        Unsupported,
    }

    fn read_u16(buf: &[u8], at: usize) -> Option<u16> {
        buf.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
        buf.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    const EOCD_SIG: u32 = 0x0605_4b50;
    const CDH_SIG: u32 = 0x0201_4b50;
    const LFH_SIG: u32 = 0x0403_4b50;

    /// Locate and parse the End-Of-Central-Directory record, then walk the
    /// central directory and return all entries.
    pub fn read_central_directory(bytes: &[u8]) -> Result<Vec<ZipEntry>, ZipError> {
        if bytes.len() < 22 {
            return Err(ZipError::Malformed);
        }
        // Scan backward for the EOCD signature (no ZIP comment search beyond a
        // bounded window).
        let max_back = bytes.len().saturating_sub(22);
        let window_start = max_back.saturating_sub(0xffff);
        let mut eocd = None;
        let mut idx = max_back;
        while idx >= window_start {
            if read_u32(bytes, idx) == Some(EOCD_SIG) {
                eocd = Some(idx);
                break;
            }
            if idx == 0 {
                break;
            }
            idx -= 1;
        }
        let eocd = eocd.ok_or(ZipError::Malformed)?;
        let total_entries = read_u16(bytes, eocd + 10).ok_or(ZipError::Malformed)? as usize;
        let cd_offset = read_u32(bytes, eocd + 16).ok_or(ZipError::Malformed)? as usize;

        let mut entries = Vec::with_capacity(total_entries);
        let mut cursor = cd_offset;
        for _ in 0..total_entries {
            if read_u32(bytes, cursor) != Some(CDH_SIG) {
                return Err(ZipError::Malformed);
            }
            let method = read_u16(bytes, cursor + 10).ok_or(ZipError::Malformed)?;
            let compressed_size = read_u32(bytes, cursor + 20).ok_or(ZipError::Malformed)? as u64;
            let uncompressed_size = read_u32(bytes, cursor + 24).ok_or(ZipError::Malformed)? as u64;
            let name_len = read_u16(bytes, cursor + 28).ok_or(ZipError::Malformed)? as usize;
            let extra_len = read_u16(bytes, cursor + 30).ok_or(ZipError::Malformed)? as usize;
            let comment_len = read_u16(bytes, cursor + 32).ok_or(ZipError::Malformed)? as usize;
            let external_attrs = read_u32(bytes, cursor + 38).ok_or(ZipError::Malformed)?;
            let local_header_offset =
                read_u32(bytes, cursor + 42).ok_or(ZipError::Malformed)? as u64;
            let name_start = cursor + 46;
            let name_bytes = bytes
                .get(name_start..name_start + name_len)
                .ok_or(ZipError::Malformed)?;
            let name = String::from_utf8_lossy(name_bytes).to_string();

            // Unix mode lives in the high 16 bits of the external attributes.
            let unix_mode = external_attrs >> 16;
            let is_symlink = unix_mode & 0o170000 == 0o120000;
            let is_executable = unix_mode & 0o111 != 0;
            let is_dir = name.ends_with('/');

            entries.push(ZipEntry {
                name,
                is_dir,
                is_symlink,
                is_executable,
                method,
                compressed_size,
                uncompressed_size,
                local_header_offset,
            });
            cursor = name_start + name_len + extra_len + comment_len;
        }
        Ok(entries)
    }

    /// Inflate one entry's bytes by reading its local file header and decoding.
    pub fn extract_entry(bytes: &[u8], entry: &ZipEntry) -> Result<Vec<u8>, ZipError> {
        let lfh = entry.local_header_offset as usize;
        if read_u32(bytes, lfh) != Some(LFH_SIG) {
            return Err(ZipError::Malformed);
        }
        let name_len = read_u16(bytes, lfh + 26).ok_or(ZipError::Malformed)? as usize;
        let extra_len = read_u16(bytes, lfh + 28).ok_or(ZipError::Malformed)? as usize;
        let data_start = lfh + 30 + name_len + extra_len;
        let data = bytes
            .get(data_start..data_start + entry.compressed_size as usize)
            .ok_or(ZipError::Malformed)?;
        match entry.method {
            0 => Ok(data.to_vec()),
            8 => inflate(data, entry.uncompressed_size as usize),
            _ => Err(ZipError::Unsupported),
        }
    }

    /// A small, allocation-bounded DEFLATE decoder (RFC 1951). Supports stored,
    /// fixed-Huffman, and dynamic-Huffman blocks. `expected` bounds the output
    /// so a malformed stream cannot exhaust memory.
    fn inflate(input: &[u8], expected: usize) -> Result<Vec<u8>, ZipError> {
        let mut reader = BitReader::new(input);
        let mut out = Vec::with_capacity(expected.min(1 << 20));
        let cap = expected.saturating_add(1);
        loop {
            let bfinal = reader.bit().ok_or(ZipError::Malformed)?;
            let btype = reader.bits(2).ok_or(ZipError::Malformed)?;
            match btype {
                0 => {
                    reader.align_to_byte();
                    let len = reader.bytes_u16().ok_or(ZipError::Malformed)?;
                    let _nlen = reader.bytes_u16().ok_or(ZipError::Malformed)?;
                    for _ in 0..len {
                        let byte = reader.raw_byte().ok_or(ZipError::Malformed)?;
                        out.push(byte);
                        if out.len() > cap {
                            return Err(ZipError::Malformed);
                        }
                    }
                }
                1 => inflate_block(&mut reader, &mut out, cap, &fixed_litlen(), &fixed_dist())?,
                2 => {
                    let (litlen, dist) = read_dynamic_tables(&mut reader)?;
                    inflate_block(&mut reader, &mut out, cap, &litlen, &dist)?;
                }
                _ => return Err(ZipError::Malformed),
            }
            if bfinal == 1 {
                break;
            }
        }
        Ok(out)
    }

    fn inflate_block(
        reader: &mut BitReader,
        out: &mut Vec<u8>,
        cap: usize,
        litlen: &Huffman,
        dist: &Huffman,
    ) -> Result<(), ZipError> {
        const LEN_BASE: [u16; 29] = [
            3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99,
            115, 131, 163, 195, 227, 258,
        ];
        const LEN_EXTRA: [u8; 29] = [
            0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
        ];
        const DIST_BASE: [u16; 30] = [
            1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025,
            1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
        ];
        const DIST_EXTRA: [u8; 30] = [
            0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12,
            12, 13, 13,
        ];
        loop {
            let sym = litlen.decode(reader).ok_or(ZipError::Malformed)?;
            if sym == 256 {
                return Ok(());
            }
            if sym < 256 {
                out.push(sym as u8);
                if out.len() > cap {
                    return Err(ZipError::Malformed);
                }
                continue;
            }
            let li = (sym - 257) as usize;
            if li >= LEN_BASE.len() {
                return Err(ZipError::Malformed);
            }
            let length = LEN_BASE[li] as usize
                + reader
                    .bits(LEN_EXTRA[li] as u32)
                    .ok_or(ZipError::Malformed)? as usize;
            let dsym = dist.decode(reader).ok_or(ZipError::Malformed)? as usize;
            if dsym >= DIST_BASE.len() {
                return Err(ZipError::Malformed);
            }
            let distance = DIST_BASE[dsym] as usize
                + reader
                    .bits(DIST_EXTRA[dsym] as u32)
                    .ok_or(ZipError::Malformed)? as usize;
            if distance == 0 || distance > out.len() {
                return Err(ZipError::Malformed);
            }
            let start = out.len() - distance;
            for i in 0..length {
                let byte = out[start + i];
                out.push(byte);
                if out.len() > cap {
                    return Err(ZipError::Malformed);
                }
            }
        }
    }

    fn read_dynamic_tables(reader: &mut BitReader) -> Result<(Huffman, Huffman), ZipError> {
        const ORDER: [usize; 19] = [
            16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
        ];
        let hlit = reader.bits(5).ok_or(ZipError::Malformed)? as usize + 257;
        let hdist = reader.bits(5).ok_or(ZipError::Malformed)? as usize + 1;
        let hclen = reader.bits(4).ok_or(ZipError::Malformed)? as usize + 4;
        let mut code_len_lengths = [0u8; 19];
        for &slot in ORDER.iter().take(hclen) {
            code_len_lengths[slot] = reader.bits(3).ok_or(ZipError::Malformed)? as u8;
        }
        let code_len_huff = Huffman::from_lengths(&code_len_lengths).ok_or(ZipError::Malformed)?;

        let total = hlit + hdist;
        let mut lengths = Vec::with_capacity(total);
        while lengths.len() < total {
            let sym = code_len_huff.decode(reader).ok_or(ZipError::Malformed)?;
            match sym {
                0..=15 => lengths.push(sym as u8),
                16 => {
                    let repeat = 3 + reader.bits(2).ok_or(ZipError::Malformed)? as usize;
                    let last = *lengths.last().ok_or(ZipError::Malformed)?;
                    for _ in 0..repeat {
                        lengths.push(last);
                    }
                }
                17 => {
                    let repeat = 3 + reader.bits(3).ok_or(ZipError::Malformed)? as usize;
                    lengths.resize(lengths.len() + repeat, 0);
                }
                18 => {
                    let repeat = 11 + reader.bits(7).ok_or(ZipError::Malformed)? as usize;
                    lengths.resize(lengths.len() + repeat, 0);
                }
                _ => return Err(ZipError::Malformed),
            }
        }
        if lengths.len() != total {
            return Err(ZipError::Malformed);
        }
        let litlen = Huffman::from_lengths(&lengths[..hlit]).ok_or(ZipError::Malformed)?;
        let dist = Huffman::from_lengths(&lengths[hlit..]).ok_or(ZipError::Malformed)?;
        Ok((litlen, dist))
    }

    fn fixed_litlen() -> Huffman {
        let mut lengths = [0u8; 288];
        for (i, slot) in lengths.iter_mut().enumerate() {
            *slot = if i < 144 {
                8
            } else if i < 256 {
                9
            } else if i < 280 {
                7
            } else {
                8
            };
        }
        Huffman::from_lengths(&lengths).expect("fixed litlen table is valid")
    }

    fn fixed_dist() -> Huffman {
        let lengths = [5u8; 30];
        Huffman::from_lengths(&lengths).expect("fixed dist table is valid")
    }

    /// A canonical-Huffman decode table built from per-symbol code lengths.
    struct Huffman {
        // (length, code) -> symbol, decoded bit-by-bit.
        counts: Vec<u16>,
        symbols: Vec<u16>,
    }

    impl Huffman {
        fn from_lengths(lengths: &[u8]) -> Option<Self> {
            let max_bits = 15usize;
            let mut counts = vec![0u16; max_bits + 1];
            for &len in lengths {
                if len as usize > max_bits {
                    return None;
                }
                counts[len as usize] += 1;
            }
            counts[0] = 0;
            // Build the symbol list ordered by (length, symbol).
            let mut offsets = vec![0u16; max_bits + 2];
            for bits in 1..=max_bits {
                offsets[bits + 1] = offsets[bits] + counts[bits];
            }
            let mut symbols = vec![0u16; lengths.len()];
            for (sym, &len) in lengths.iter().enumerate() {
                if len != 0 {
                    let slot = offsets[len as usize] as usize;
                    symbols[slot] = sym as u16;
                    offsets[len as usize] += 1;
                }
            }
            Some(Self { counts, symbols })
        }

        fn decode(&self, reader: &mut BitReader) -> Option<u16> {
            let mut code: i32 = 0;
            let mut first: i32 = 0;
            let mut index: i32 = 0;
            for len in 1..=15usize {
                let bit = reader.bit()? as i32;
                code |= bit;
                let count = self.counts[len] as i32;
                if code - first < count {
                    let sym_index = (index + (code - first)) as usize;
                    return self.symbols.get(sym_index).copied();
                }
                index += count;
                first += count;
                first <<= 1;
                code <<= 1;
            }
            None
        }
    }

    /// LSB-first bit reader over a byte slice (DEFLATE bit order).
    struct BitReader<'a> {
        data: &'a [u8],
        byte_pos: usize,
        bit_pos: u32,
    }

    impl<'a> BitReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self {
                data,
                byte_pos: 0,
                bit_pos: 0,
            }
        }

        fn bit(&mut self) -> Option<u32> {
            let byte = *self.data.get(self.byte_pos)?;
            let bit = (byte >> self.bit_pos) & 1;
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
            Some(bit as u32)
        }

        fn bits(&mut self, count: u32) -> Option<u32> {
            let mut value = 0u32;
            for i in 0..count {
                let bit = self.bit()?;
                value |= bit << i;
            }
            Some(value)
        }

        fn align_to_byte(&mut self) {
            if self.bit_pos != 0 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }

        fn raw_byte(&mut self) -> Option<u8> {
            let byte = *self.data.get(self.byte_pos)?;
            self.byte_pos += 1;
            Some(byte)
        }

        fn bytes_u16(&mut self) -> Option<u16> {
            let lo = self.raw_byte()? as u16;
            let hi = self.raw_byte()? as u16;
            Some(lo | (hi << 8))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{PackageProvenance, content_hash};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A self-cleaning temp directory rooted under the OS temp dir.
    struct TempDir {
        root: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let mut root = std::env::temp_dir();
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            root.push(format!(
                "opensks-design-import-{name}-{}-{stamp}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create temp dir");
            Self {
                root: root.canonicalize().expect("canonicalize temp dir"),
            }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn write(&self, relative: &str, contents: &[u8]) {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(path, contents).expect("write fixture");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Build the bytes of a clean package's three files (manifest/tokens/design)
    /// with valid content hashes bound to `id`.
    fn clean_package_files(id: &str, license: &str) -> Vec<(String, Vec<u8>)> {
        let tokens = format!(
            "{{\"schema\":\"opensks.design-token-set.v1\",\"design_system_id\":\"{id}\",\"revision\":1,\"tokens\":[{{\"path\":\"color.canvas\",\"type\":\"color\",\"value\":\"#000000\"}}]}}"
        );
        let design = "# Title\n\n## Section\n\nBody\n".to_string();
        let tokens_hash = content_hash(tokens.as_bytes());
        let design_hash = content_hash(design.as_bytes());
        let manifest = format!(
            "{{\"schema\":\"opensks.design-package.v1\",\"id\":\"{id}\",\"name\":\"{id}\",\"version\":\"1.0.0\",\"license\":\"{license}\",\"description\":\"d\",\"package_schema_version\":1,\"files\":{{\"design\":\"DESIGN.md\",\"tokens\":\"tokens.json\"}},\"content_hashes\":[{{\"path\":\"tokens.json\",\"hash\":\"{tokens_hash}\"}},{{\"path\":\"DESIGN.md\",\"hash\":\"{design_hash}\"}}],\"platforms\":[\"macos-swiftui\"]}}"
        );
        vec![
            ("manifest.json".to_string(), manifest.into_bytes()),
            ("tokens.json".to_string(), tokens.into_bytes()),
            ("DESIGN.md".to_string(), design.into_bytes()),
        ]
    }

    /// Write a clean package directory under `dir/<rel>/`.
    fn write_clean_package_dir(dir: &TempDir, rel: &str, id: &str, license: &str) {
        for (name, bytes) in clean_package_files(id, license) {
            dir.write(&format!("{rel}/{name}"), &bytes);
        }
    }

    /// Minimal STORED-method ZIP writer for test fixtures. Each `(name, bytes,
    /// unix_mode)` becomes one entry. `unix_mode` controls the external file
    /// attributes (so we can forge an executable bit or an S_IFLNK symlink).
    fn build_zip(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc: u32 = 0xffff_ffff;
            for &b in bytes {
                crc ^= b as u32;
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
                }
            }
            !crc
        }
        let mut out = Vec::new();
        let mut central = Vec::new();
        let mut offsets = Vec::new();
        for (name, data, _mode) in entries {
            offsets.push(out.len() as u32);
            let crc = crc32(data);
            let name_bytes = name.as_bytes();
            // Local file header.
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method = stored
            out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            out.extend_from_slice(&0u16.to_le_bytes()); // mod date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // compressed
            out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // uncompressed
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(data);
        }
        for ((name, data, mode), offset) in entries.iter().zip(offsets.iter()) {
            let crc = crc32(data);
            let name_bytes = name.as_bytes();
            let external_attrs = mode << 16;
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes()); // version made by
            central.extend_from_slice(&20u16.to_le_bytes()); // version needed
            central.extend_from_slice(&0u16.to_le_bytes()); // flags
            central.extend_from_slice(&0u16.to_le_bytes()); // method = stored
            central.extend_from_slice(&0u16.to_le_bytes()); // mod time
            central.extend_from_slice(&0u16.to_le_bytes()); // mod date
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // extra len
            central.extend_from_slice(&0u16.to_le_bytes()); // comment len
            central.extend_from_slice(&0u16.to_le_bytes()); // disk number
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            central.extend_from_slice(&external_attrs.to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name_bytes);
        }
        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        // End of central directory.
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number
        out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }

    const MODE_REG: u32 = 0o100644;
    const MODE_EXEC: u32 = 0o100755;
    const MODE_SYMLINK: u32 = 0o120777;

    // ---- ZIP-SLIP ---------------------------------------------------------

    #[test]
    fn zip_slip_entry_is_blocked_and_writes_nothing_outside() {
        let dir = TempDir::new("zipslip");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        // A canary file outside the quarantine that a `../` escape would target.
        let outside = dir.path("escape-target.txt");
        fs::write(&outside, b"original").unwrap();

        let zip = build_zip(&[
            ("manifest.json", b"{}", MODE_REG),
            ("../../../../../../escape-target.txt", b"PWNED", MODE_REG),
        ]);
        let src = dir.path("evil.zip");
        fs::write(&src, &zip).unwrap();

        let outcome =
            quarantine_import(&ws, &src, ImportKind::Archive, &ImportLimits::default()).unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Rejected);
        assert_eq!(outcome.entry.rejected_reason, Some(RejectedReason::ZipSlip));
        // The outside canary is untouched: nothing was written outside quarantine.
        assert_eq!(fs::read(&outside).unwrap(), b"original");
        // And the payload was scrubbed.
        assert!(!outcome.quarantine_dir.join(PAYLOAD_DIR).exists());
    }

    // ---- SYMLINK ----------------------------------------------------------

    #[test]
    fn archive_symlink_entry_is_blocked() {
        let dir = TempDir::new("ziplink");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let zip = build_zip(&[
            ("manifest.json", b"{}", MODE_REG),
            ("evil-link", b"/etc/passwd", MODE_SYMLINK),
        ]);
        let src = dir.path("link.zip");
        fs::write(&src, &zip).unwrap();
        let outcome =
            quarantine_import(&ws, &src, ImportKind::Archive, &ImportLimits::default()).unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Rejected);
        assert_eq!(outcome.entry.rejected_reason, Some(RejectedReason::Symlink));
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlink_file_is_blocked() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new("dirlink");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let pkg = dir.path("pkg");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("manifest.json"), b"{}").unwrap();
        // A symlink inside the source dir must be rejected (never followed).
        fs::write(dir.path("secret.txt"), b"secret").unwrap();
        symlink(dir.path("secret.txt"), pkg.join("link.json")).unwrap();

        let outcome =
            quarantine_import(&ws, &pkg, ImportKind::Local, &ImportLimits::default()).unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Rejected);
        assert_eq!(outcome.entry.rejected_reason, Some(RejectedReason::Symlink));
    }

    // ---- EXECUTABLE / SCRIPT ---------------------------------------------

    #[test]
    fn archive_executable_bit_is_blocked() {
        let dir = TempDir::new("zipexec");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        // A .json with an executable bit is still rejected as executable.
        let zip = build_zip(&[
            ("manifest.json", b"{}", MODE_REG),
            ("tool.json", b"{\"ok\":true}", MODE_EXEC),
        ]);
        let src = dir.path("exec.zip");
        fs::write(&src, &zip).unwrap();
        let outcome =
            quarantine_import(&ws, &src, ImportKind::Archive, &ImportLimits::default()).unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Rejected);
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::ExecutableOrScript)
        );
    }

    #[test]
    fn script_extension_is_blocked() {
        let dir = TempDir::new("zipsh");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let zip = build_zip(&[
            ("manifest.json", b"{}", MODE_REG),
            // A `.sh` script is rejected by extension + shebang, regardless of
            // its body, so the fixture keeps a harmless payload (no destructive
            // literal that would trip the repo's own security surface scanner).
            ("install.sh", b"#!/bin/sh\necho hello\n", MODE_REG),
        ]);
        let src = dir.path("script.zip");
        fs::write(&src, &zip).unwrap();
        let outcome =
            quarantine_import(&ws, &src, ImportKind::Archive, &ImportLimits::default()).unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Rejected);
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::ExecutableOrScript)
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_exec_bit_file_is_blocked() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("localexec");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let pkg = dir.path("pkg");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("manifest.json"), b"{}").unwrap();
        let exe = pkg.join("payload.json");
        fs::write(&exe, b"{}").unwrap();
        let mut perms = fs::metadata(&exe).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&exe, perms).unwrap();

        let outcome =
            quarantine_import(&ws, &pkg, ImportKind::Local, &ImportLimits::default()).unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Rejected);
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::ExecutableOrScript)
        );
    }

    #[test]
    fn shebang_in_allowed_extension_is_blocked() {
        let dir = TempDir::new("shebang");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        // A .txt file that smuggles a shebang.
        let zip = build_zip(&[
            ("manifest.json", b"{}", MODE_REG),
            ("notes.txt", b"#!/usr/bin/env python\nprint(1)\n", MODE_REG),
        ]);
        let src = dir.path("shebang.zip");
        fs::write(&src, &zip).unwrap();
        let outcome =
            quarantine_import(&ws, &src, ImportKind::Archive, &ImportLimits::default()).unwrap();
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::ExecutableOrScript)
        );
    }

    // ---- LIMITS -----------------------------------------------------------

    #[test]
    fn too_many_files_is_blocked() {
        let dir = TempDir::new("manyfiles");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let pkg = dir.path("pkg");
        fs::create_dir_all(&pkg).unwrap();
        for i in 0..10 {
            fs::write(pkg.join(format!("f{i}.json")), b"{}").unwrap();
        }
        let limits = ImportLimits {
            max_file_count: 3,
            ..ImportLimits::default()
        };
        let outcome = quarantine_import(&ws, &pkg, ImportKind::Local, &limits).unwrap();
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::TooManyFiles)
        );
    }

    #[test]
    fn too_large_total_is_blocked() {
        let dir = TempDir::new("toolarge");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let pkg = dir.path("pkg");
        fs::create_dir_all(&pkg).unwrap();
        let big = vec![b'a'; 1024];
        for i in 0..10 {
            fs::write(pkg.join(format!("f{i}.json")), &big).unwrap();
        }
        let limits = ImportLimits {
            max_total_bytes: 2048,
            ..ImportLimits::default()
        };
        let outcome = quarantine_import(&ws, &pkg, ImportKind::Local, &limits).unwrap();
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::TooLarge)
        );
    }

    #[test]
    fn per_entry_zip_bomb_cap_is_blocked() {
        let dir = TempDir::new("bomb");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let big = vec![b'a'; 4096];
        let zip = build_zip(&[
            ("manifest.json", b"{}", MODE_REG),
            ("huge.json", &big, MODE_REG),
        ]);
        let src = dir.path("bomb.zip");
        fs::write(&src, &zip).unwrap();
        let limits = ImportLimits {
            max_entry_bytes: 1024,
            ..ImportLimits::default()
        };
        let outcome = quarantine_import(&ws, &src, ImportKind::Archive, &limits).unwrap();
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::TooLarge)
        );
    }

    #[test]
    fn too_many_archive_entries_is_blocked() {
        let dir = TempDir::new("manyentries");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let names: Vec<String> = (0..10).map(|i| format!("f{i}.json")).collect();
        let entries: Vec<(&str, &[u8], u32)> = names
            .iter()
            .map(|n| (n.as_str(), b"{}".as_slice(), MODE_REG))
            .collect();
        let zip = build_zip(&entries);
        let src = dir.path("many.zip");
        fs::write(&src, &zip).unwrap();
        let limits = ImportLimits {
            max_archive_entries: 3,
            ..ImportLimits::default()
        };
        let outcome = quarantine_import(&ws, &src, ImportKind::Archive, &limits).unwrap();
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::TooManyArchiveEntries)
        );
    }

    // ---- MIME -------------------------------------------------------------

    #[test]
    fn disallowed_mime_is_blocked() {
        let dir = TempDir::new("mime");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let pkg = dir.path("pkg");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(pkg.join("manifest.json"), b"{}").unwrap();
        // An unknown binary extension that is not script/exec but not allowed.
        fs::write(pkg.join("data.dat"), b"\x00\x01\x02binary").unwrap();
        let outcome =
            quarantine_import(&ws, &pkg, ImportKind::Local, &ImportLimits::default()).unwrap();
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::MimeNotAllowed)
        );
    }

    #[test]
    fn exe_extension_is_blocked() {
        let dir = TempDir::new("exe");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let zip = build_zip(&[
            ("manifest.json", b"{}", MODE_REG),
            ("payload.exe", b"MZ\x90\x00binary", MODE_REG),
        ]);
        let src = dir.path("evil-exe.zip");
        fs::write(&src, &zip).unwrap();
        let outcome =
            quarantine_import(&ws, &src, ImportKind::Archive, &ImportLimits::default()).unwrap();
        // .exe is on the script/binary list, so it is executable_or_script.
        assert_eq!(
            outcome.entry.rejected_reason,
            Some(RejectedReason::ExecutableOrScript)
        );
    }

    // ---- CLEAN QUARANTINE + PROVENANCE -----------------------------------

    #[test]
    fn clean_directory_quarantines_with_provenance_and_count() {
        let dir = TempDir::new("clean-dir");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        write_clean_package_dir(&dir, "pkg", "demo", "MIT");
        let outcome = quarantine_import(
            &ws,
            &dir.path("pkg"),
            ImportKind::Local,
            &ImportLimits::default(),
        )
        .unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Quarantined);
        assert_eq!(outcome.entry.rejected_reason, None);
        assert_eq!(outcome.entry.file_count, 3);
        assert_eq!(outcome.entry.provenance.source, "pkg");
        assert_eq!(
            outcome.entry.provenance.license,
            Some("MIT".to_string()),
            "license must be read from the package manifest"
        );
        assert_eq!(outcome.entry.provenance.commit, None);
        // Payload is on disk inside the quarantine dir.
        assert!(
            outcome
                .quarantine_dir
                .join(PAYLOAD_DIR)
                .join("manifest.json")
                .exists()
        );
        // The persisted entry round-trips and shows in status.
        let listed = list_quarantines(&ws).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].quarantine_id, outcome.entry.quarantine_id);
        let status_json = render_status_json(&listed);
        assert!(status_json.contains("\"opensks.design-import-status.v1\""));
        assert!(status_json.contains("\"license\":\"MIT\""));
    }

    #[test]
    fn clean_archive_quarantines() {
        let dir = TempDir::new("clean-zip");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let files = clean_package_files("demo", "Apache-2.0");
        let entries: Vec<(&str, &[u8], u32)> = files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice(), MODE_REG))
            .collect();
        let zip = build_zip(&entries);
        let src = dir.path("clean.zip");
        fs::write(&src, &zip).unwrap();
        let outcome =
            quarantine_import(&ws, &src, ImportKind::Archive, &ImportLimits::default()).unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Quarantined);
        assert_eq!(outcome.entry.file_count, 3);
        assert_eq!(
            outcome.entry.provenance.license,
            Some("Apache-2.0".to_string())
        );
    }

    // ---- APPROVE (promotion) ---------------------------------------------

    #[test]
    fn approve_promotes_clean_package_and_it_resolves() {
        let dir = TempDir::new("approve");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        write_clean_package_dir(&dir, "pkg", "demo", "MIT");
        let outcome = quarantine_import(
            &ws,
            &dir.path("pkg"),
            ImportKind::Local,
            &ImportLimits::default(),
        )
        .unwrap();
        assert_eq!(outcome.entry.status, QuarantineStatus::Quarantined);

        // Promotion is a separate, explicit step.
        let approved = approve_import(&ws, &outcome.entry.quarantine_id).unwrap();
        assert!(approved.promoted);
        assert_eq!(approved.package_id, "demo");

        // The promoted package resolves via the PR-037 registry.
        let registry = DesignRegistry::with_default_order(&ws, None);
        let resolved = registry.resolve("demo").expect("promoted package resolves");
        assert_eq!(resolved.manifest.id, "demo");
        assert_eq!(resolved.provenance, PackageProvenance::Local);

        // Re-approving the same id refuses to clobber.
        let outcome2 = quarantine_import(
            &ws,
            &dir.path("pkg"),
            ImportKind::Local,
            &ImportLimits::default(),
        )
        .unwrap();
        let err = approve_import(&ws, &outcome2.entry.quarantine_id).unwrap_err();
        assert!(matches!(err, ImportError::AlreadyPromoted { .. }));
    }

    #[test]
    fn approve_revalidates_and_rejects_tampered_payload() {
        let dir = TempDir::new("approve-tamper");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        write_clean_package_dir(&dir, "pkg", "demo", "MIT");
        let outcome = quarantine_import(
            &ws,
            &dir.path("pkg"),
            ImportKind::Local,
            &ImportLimits::default(),
        )
        .unwrap();
        // Tamper with the quarantined tokens.json AFTER quarantine, so the
        // declared content hash no longer matches. Approval must re-validate and
        // refuse to promote.
        let tokens = outcome.quarantine_dir.join(PAYLOAD_DIR).join("tokens.json");
        fs::write(&tokens, b"{\"schema\":\"opensks.design-token-set.v1\",\"design_system_id\":\"demo\",\"revision\":99,\"tokens\":[]}").unwrap();
        let err = approve_import(&ws, &outcome.entry.quarantine_id).unwrap_err();
        assert!(matches!(err, ImportError::Validation(_)));
        // Nothing was promoted.
        assert!(!design_systems_root(&ws).join("demo").exists());
    }

    // ---- REJECT (safe delete) --------------------------------------------

    #[test]
    fn reject_deletes_quarantine_dir() {
        let dir = TempDir::new("reject");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        write_clean_package_dir(&dir, "pkg", "demo", "MIT");
        let outcome = quarantine_import(
            &ws,
            &dir.path("pkg"),
            ImportKind::Local,
            &ImportLimits::default(),
        )
        .unwrap();
        assert!(outcome.quarantine_dir.exists());
        let deleted = reject_import(&ws, &outcome.entry.quarantine_id).unwrap();
        assert!(deleted);
        assert!(!outcome.quarantine_dir.exists());
        assert!(list_quarantines(&ws).unwrap().is_empty());
    }

    #[test]
    fn reject_never_deletes_outside_the_quarantine_root() {
        let dir = TempDir::new("reject-escape");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        // A precious dir OUTSIDE the quarantine root.
        let precious = dir.path("precious");
        fs::create_dir_all(&precious).unwrap();
        fs::write(precious.join("keep.txt"), b"keep").unwrap();

        // A traversal id is rejected as not-found before any delete.
        let err = reject_import(&ws, "../../../precious").unwrap_err();
        assert!(matches!(err, ImportError::QuarantineNotFound { .. }));
        assert!(precious.join("keep.txt").exists(), "outside dir untouched");

        // Even if an attacker plants a symlink quarantine dir pointing outside,
        // the canonicalized-containment guard refuses to remove it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let root = quarantine_root(&ws);
            fs::create_dir_all(&root).unwrap();
            let link = root.join("q-evil");
            symlink(&precious, &link).unwrap();
            // Plant a sidecar so locate_quarantine finds it via the link.
            fs::write(link.join(ENTRY_FILE_NAME), b"{}").ok();
            let _ = reject_import(&ws, "q-evil");
            // The precious dir and its contents survive.
            assert!(
                precious.join("keep.txt").exists(),
                "symlinked quarantine must not delete outside the root"
            );
        }
    }

    #[test]
    fn missing_source_is_a_hard_error_not_a_rejection() {
        let dir = TempDir::new("missing");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        let err = quarantine_import(
            &ws,
            &dir.path("does-not-exist"),
            ImportKind::Local,
            &ImportLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ImportError::SourceMissing { .. }));
    }

    // ---- NO NETWORK -------------------------------------------------------

    #[test]
    fn import_module_source_has_no_network_or_process_spawn() {
        // Static guard: the import implementation must not reference any network
        // client, std::net, or process spawning of network tools. This asserts
        // the "LOCAL ONLY, no upload, no fetch" invariant at the source level.
        let source = include_str!("import.rs");
        // Only inspect the implementation, not this test module's assertion text.
        let impl_src = source
            .split("#[cfg(test)]")
            .next()
            .expect("module has a pre-test section");
        for needle in [
            "std::net",
            "TcpStream",
            "reqwest",
            "ureq",
            "hyper::",
            "Command::new",
            "std::process::Command",
            "http://",
            "https://",
        ] {
            assert!(
                !impl_src.contains(needle),
                "import implementation must not reference `{needle}` (network/process spawn)"
            );
        }
    }

    // ---- ZIP reader sanity (stored + deflate) ----------------------------

    #[test]
    fn zip_reader_reads_stored_entries() {
        let zip = build_zip(&[("a.json", b"{\"x\":1}", MODE_REG)]);
        let entries = zip::read_central_directory(&zip).expect("read cd");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.json");
        let data = zip::extract_entry(&zip, &entries[0]).expect("extract");
        assert_eq!(data, b"{\"x\":1}");
    }

    // ======================================================================
    // PR-044 PART B — FUZZ CORPUS (design-import / preview ingress)
    //
    // Deterministic, in-process, dependency-free fuzzing over the untrusted
    // design-package parsers: the ZIP central-directory reader, the per-entry
    // DEFLATE inflater, the archive-path normalizer, and the full
    // `quarantine_import` ingress for both archive and directory kinds. The
    // corpus is a fixed seed list of adversarial byte vectors expanded with a
    // tiny deterministic LCG mutator (no external fuzzer, no network, no
    // randomness from the clock). For EVERY case we assert three invariants:
    //
    //   1. NEVER PANICS  — the parser returns a typed Result/Option; the test
    //      harness itself would unwind on a panic, failing the suite.
    //   2. NEVER ESCAPES — no byte is ever written outside the quarantine
    //      payload dir, and an outside canary file is byte-for-byte intact.
    //   3. TYPED OUTCOME — a malformed/adversarial input yields either a
    //      typed error or a `Rejected` quarantine entry with a stable reason,
    //      never an uncontained success.
    // ======================================================================

    /// A small deterministic PRNG (xorshift64*) so the corpus is identical on
    /// every run and every machine — no clock, no OS entropy.
    struct Lcg(u64);
    impl Lcg {
        fn new(seed: u64) -> Self {
            // Avoid the zero fixed-point of xorshift.
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
        fn byte(&mut self) -> u8 {
            (self.next_u64() >> 33) as u8
        }
    }

    /// The fixed seed list of adversarial byte vectors fed to the raw parsers.
    /// Each entry exercises a distinct malformed/hostile class.
    fn adversarial_seeds() -> Vec<Vec<u8>> {
        let mut seeds: Vec<Vec<u8>> = vec![
            // empty / too short for an EOCD
            vec![],
            vec![0x50],
            vec![0x50, 0x4b],
            b"PK\x05\x06".to_vec(), // EOCD signature, but truncated record
            // truncated archives
            b"PK\x03\x04".to_vec(),
            b"PK\x03\x04\x14\x00\x00\x00".to_vec(),
            // a lying EOCD claiming a huge entry count
            {
                let mut v = vec![0u8; 22];
                v[0..4].copy_from_slice(&0x0605_4b50u32.to_le_bytes());
                v[10..12].copy_from_slice(&0xffffu16.to_le_bytes()); // total entries
                v[16..20].copy_from_slice(&0x0000_0000u32.to_le_bytes()); // cd offset 0
                v
            },
            // EOCD with a central-directory offset pointing past the buffer
            {
                let mut v = vec![0u8; 22];
                v[0..4].copy_from_slice(&0x0605_4b50u32.to_le_bytes());
                v[10..12].copy_from_slice(&1u16.to_le_bytes());
                v[16..20].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
                v
            },
            // all-zero / all-0xFF blobs of various sizes
            vec![0u8; 64],
            vec![0xffu8; 64],
            vec![0u8; 4096],
            // NUL bytes and non-UTF8 noise
            vec![0x00, 0x01, 0x02, 0x00, 0xff, 0xfe, 0x80, 0x81],
            (0u8..=255).collect::<Vec<u8>>(),
            // a real but tiny stored zip we then mutate
            build_zip(&[("manifest.json", b"{}", MODE_REG)]),
            build_zip(&[("a.json", b"{\"x\":1}", MODE_REG)]),
            // a zip whose entry name is a path-traversal / absolute / drive / UNC
            build_zip(&[("../../etc/passwd", b"x", MODE_REG)]),
            build_zip(&[("/abs/secret", b"x", MODE_REG)]),
            build_zip(&[("C:\\windows\\system32", b"x", MODE_REG)]),
            build_zip(&[("a/b/../../../../escape", b"x", MODE_REG)]),
            // a zip with a NUL inside the entry name
            build_zip(&[("na\0me.json", b"x", MODE_REG)]),
            // a symlink-moded entry and an exec-moded entry
            build_zip(&[("link", b"/etc/passwd", MODE_SYMLINK)]),
            build_zip(&[("tool.json", b"{}", MODE_EXEC)]),
            // an entry whose declared (central-dir) size lies about the data
            {
                let mut z = build_zip(&[("big.json", b"{}", MODE_REG)]);
                // smash the uncompressed-size field of the first CDH if we can
                // find the signature; harmless if not present.
                if let Some(pos) = find_subslice(&z, &0x0201_4b50u32.to_le_bytes()) {
                    let off = pos + 24;
                    if off + 4 <= z.len() {
                        z[off..off + 4].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
                    }
                }
                z
            },
        ];
        // Deeply nested archive entry name (path-depth bomb).
        let deep = format!("{}leaf.json", "a/".repeat(512));
        seeds.push(build_zip(&[(deep.as_str(), b"{}", MODE_REG)]));
        // A bogus-MIME but otherwise clean entry.
        seeds.push(build_zip(&[("evil.dat", b"\x00\x01binary", MODE_REG)]));
        seeds
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[test]
    fn fuzz_zip_central_directory_reader_never_panics() {
        // Feed every seed plus hundreds of deterministic mutations through the
        // raw central-directory reader and the per-entry extractor. The only
        // contract is: return a typed Result, never panic / hang / OOM.
        let mut cases = 0usize;
        for seed in adversarial_seeds() {
            let mut rng = Lcg::new(0xA5A5_5A5A ^ seed.len() as u64);
            for _ in 0..40 {
                let mut bytes = seed.clone();
                mutate(&mut bytes, &mut rng);
                cases += 1;
                // Reader must not panic; on success, extracting each entry must
                // also not panic (bounded by the inflater's output cap).
                if let Ok(entries) = zip::read_central_directory(&bytes) {
                    for entry in entries.iter().take(64) {
                        let _ = zip::extract_entry(&bytes, entry);
                    }
                }
            }
        }
        assert!(cases >= 400, "fuzzed {cases} central-directory cases");
    }

    /// Apply a few deterministic in-place mutations (bit flips, truncations,
    /// signature smashes) to a byte vector.
    fn mutate(bytes: &mut Vec<u8>, rng: &mut Lcg) {
        if bytes.is_empty() {
            bytes.push(rng.byte());
            return;
        }
        let ops = (rng.next_u64() % 4) + 1;
        for _ in 0..ops {
            match rng.next_u64() % 5 {
                0 => {
                    // flip a byte
                    let i = (rng.next_u64() as usize) % bytes.len();
                    bytes[i] ^= rng.byte();
                }
                1 => {
                    // truncate
                    let keep = (rng.next_u64() as usize) % bytes.len();
                    bytes.truncate(keep);
                    if bytes.is_empty() {
                        bytes.push(rng.byte());
                    }
                }
                2 => {
                    // append junk
                    for _ in 0..(rng.next_u64() % 17) {
                        bytes.push(rng.byte());
                    }
                }
                3 => {
                    // smash a 4-byte window (often a signature)
                    if bytes.len() >= 4 {
                        let i = (rng.next_u64() as usize) % (bytes.len() - 3);
                        for b in &mut bytes[i..i + 4] {
                            *b = rng.byte();
                        }
                    }
                }
                _ => {
                    // overwrite a run with zeros
                    let i = (rng.next_u64() as usize) % bytes.len();
                    let n = ((rng.next_u64() as usize) % 8).min(bytes.len() - i);
                    for b in &mut bytes[i..i + n] {
                        *b = 0;
                    }
                }
            }
        }
    }

    #[test]
    fn fuzz_archive_path_normalizer_never_escapes() {
        // The path normalizer is the zip-slip gate. For a hostile corpus of
        // names it must either reject (None) or return a strictly-relative,
        // `..`-free, non-absolute path — never one that climbs out.
        let names: Vec<String> = vec![
            "".into(),
            ".".into(),
            "..".into(),
            "../x".into(),
            "../../../../etc/passwd".into(),
            "/abs".into(),
            "//server/share".into(),
            "C:/win".into(),
            "C:\\win".into(),
            "\\\\unc\\share".into(),
            "a/./b/../c".into(),
            "a/../../b".into(),
            "ok/dir/file.json".into(),
            "name\0with\0nul".into(),
            "....//....//x".into(),
            "a/".repeat(1000) + "leaf",
            "x".repeat(4096),
        ];
        let mut rng = Lcg::new(0xDEAD_BEEF);
        let mut cases = 0usize;
        for base in &names {
            for _ in 0..30 {
                // Mutate the name string's bytes into new adversarial variants.
                let mut raw = base.clone().into_bytes();
                mutate(&mut raw, &mut rng);
                let name = String::from_utf8_lossy(&raw).to_string();
                cases += 1;
                if let Some(normalized) = normalize_archive_path(&name) {
                    // The result must contain no parent/root/prefix components
                    // and must not be absolute.
                    assert!(
                        !normalized.is_absolute(),
                        "normalized path is absolute: {name:?}"
                    );
                    for component in normalized.components() {
                        assert!(
                            matches!(component, Component::Normal(_)),
                            "normalized path has a non-Normal component for {name:?}"
                        );
                    }
                }
            }
        }
        assert!(cases >= 400, "fuzzed {cases} archive-path cases");
    }

    #[test]
    fn fuzz_quarantine_import_archive_never_escapes_workspace() {
        // End-to-end ingress fuzz: hand `quarantine_import` adversarial archive
        // bytes and assert (a) it never panics, (b) it never writes outside the
        // quarantine payload dir (an outside canary is untouched), and (c) the
        // outcome is always typed — a clean `Quarantined` OR a `Rejected` with a
        // stable reason, OR a hard `Err`.
        let dir = TempDir::new("fuzz-archive");
        let ws = dir.path("ws");
        fs::create_dir_all(&ws).unwrap();
        // Canary tree the malicious `../` entries try to reach.
        let canary = dir.path("OUTSIDE-CANARY.txt");
        fs::write(&canary, b"untouched").unwrap();

        let mut rng = Lcg::new(0x1234_5678_9abc_def0);
        let mut cases = 0usize;
        for seed in adversarial_seeds() {
            for _ in 0..20 {
                let mut bytes = seed.clone();
                mutate(&mut bytes, &mut rng);
                let src = dir.path(&format!("case-{cases}.zip"));
                fs::write(&src, &bytes).unwrap();
                cases += 1;
                match quarantine_import(&ws, &src, ImportKind::Archive, &ImportLimits::default()) {
                    Ok(outcome) => {
                        // A clean quarantine keeps its payload INSIDE the dir; a
                        // rejection scrubs the payload. Either way nothing lands
                        // outside.
                        match outcome.entry.status {
                            QuarantineStatus::Quarantined => {
                                assert!(outcome.entry.rejected_reason.is_none());
                            }
                            QuarantineStatus::Rejected => {
                                assert!(outcome.entry.rejected_reason.is_some());
                            }
                        }
                    }
                    Err(_) => { /* typed operator error is acceptable */ }
                }
                // INVARIANT: the canary outside the workspace is byte-intact and
                // no file leaked next to it.
                assert_eq!(
                    fs::read(&canary).unwrap(),
                    b"untouched",
                    "an archive entry escaped the quarantine and wrote outside"
                );
                let _ = fs::remove_file(&src);
            }
        }
        // Nothing was ever written into the canary's parent except our cases.
        assert!(cases >= 400, "fuzzed {cases} archive-ingress cases");
    }

    #[test]
    fn fuzz_entry_json_parser_never_panics() {
        // The persisted-entry parser (`QuarantineEntry::from_json`) is fed
        // arbitrary bytes; it must always return a typed Result, never panic.
        let seeds: Vec<&[u8]> = vec![
            b"",
            b"{",
            b"}",
            b"null",
            b"[]",
            b"{\"status\":\"quarantined\"}",
            b"{\"quarantine_id\":\"q-1\",\"status\":\"bogus\",\"kind\":\"local\"}",
            b"\xff\xfe\x00bad",
            b"{\"provenance\":{}}",
        ];
        let mut rng = Lcg::new(0xC0FF_EE00);
        let mut cases = 0usize;
        for seed in seeds {
            for _ in 0..50 {
                let mut bytes = seed.to_vec();
                mutate(&mut bytes, &mut rng);
                let text = String::from_utf8_lossy(&bytes);
                cases += 1;
                let _ = QuarantineEntry::from_json(&text);
            }
        }
        assert!(cases >= 400, "fuzzed {cases} entry-json cases");
    }
}
