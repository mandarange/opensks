//! Design-package registry, strict validation, and legacy normalization (PR-037).
//!
//! This module turns the portable [`DesignPackageManifest`] contract into a
//! discoverable, trustable artifact. It does three things:
//!
//! 1. **Discovery in a defined search order.** [`DesignRegistry`] scans a fixed
//!    ordered list of roots — the local workspace
//!    `.opensks/design-systems/` first, then a shared location — and resolves a
//!    package by id, reporting [`PackageProvenance`] (`Local` vs `Shared`). The
//!    first root in the order that contains the id wins, so a local override
//!    shadows a shared package of the same id.
//!
//! 2. **Strict path / hash / license / symlink validation.** Every path a
//!    manifest references must be package-relative, canonicalize *inside* the
//!    package directory, and not be (or traverse) a symlink. This reuses the
//!    file-service hardening order: syntactic rejection of absolute paths and
//!    `..`, then symlink rejection (via `symlink_metadata`, never following
//!    links), then canonical containment. Declared `content_hashes` are
//!    recomputed from the on-disk bytes (`fnv1a64:`) and must match. The
//!    `license` field must be present and non-empty.
//!
//! 3. **Legacy `DESIGN.md` normalization.** [`normalize_legacy_design`] reads an
//!    old free-form `DESIGN.md` and produces a [`NormalizedDesign`] with a stable
//!    title and section map, so a legacy file can be folded into a package's
//!    `DESIGN` shape without losing structure.
//!
//! Invariant: validation errors never embed file contents — only package ids,
//! package-relative paths, and stable reason codes.

use std::fs;
use std::path::{Component, Path, PathBuf};

use opensks_contracts::{
    DESIGN_PACKAGE_COMPONENTS_SCHEMA, DESIGN_PACKAGE_MANIFEST_SCHEMA, DESIGN_PACKAGE_TOKENS_SCHEMA,
    DesignPackageComponents, DesignPackageManifest, DesignPackageTokens,
};

/// Manifest file name inside every package directory.
pub const MANIFEST_FILE_NAME: &str = "manifest.json";

/// Where a resolved package was discovered, in search-order precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageProvenance {
    /// Found in the local workspace `.opensks/design-systems/` root (highest
    /// precedence; shadows a shared package of the same id).
    Local,
    /// Found in the shared/system root.
    Shared,
}

impl PackageProvenance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Shared => "shared",
        }
    }
}

/// A discovered-and-validated package: its manifest, the directory it lives in,
/// and where it was found in the search order.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub manifest: DesignPackageManifest,
    pub package_dir: PathBuf,
    pub provenance: PackageProvenance,
}

impl ResolvedPackage {
    /// Load and validate the package's token document.
    pub fn load_tokens(&self) -> Result<DesignPackageTokens, DesignRegistryError> {
        let rel = self.manifest.files.tokens.clone();
        let resolved = validate_relative_file(&self.manifest.id, &self.package_dir, &rel)?;
        let bytes = read_file(&self.manifest.id, &rel, &resolved)?;
        let tokens: DesignPackageTokens =
            serde_json::from_slice(&bytes).map_err(|_| DesignRegistryError::TokensInvalid {
                id: self.manifest.id.clone(),
            })?;
        if tokens.schema != DESIGN_PACKAGE_TOKENS_SCHEMA {
            return Err(DesignRegistryError::TokensInvalid {
                id: self.manifest.id.clone(),
            });
        }
        if tokens.design_system_id != self.manifest.id {
            return Err(DesignRegistryError::TokensIdMismatch {
                id: self.manifest.id.clone(),
                tokens_id: tokens.design_system_id.clone(),
            });
        }
        Ok(tokens)
    }

    /// Load and validate the package's component catalog, if one is declared.
    pub fn load_components(&self) -> Result<Option<DesignPackageComponents>, DesignRegistryError> {
        let Some(rel) = self.manifest.files.components.clone() else {
            return Ok(None);
        };
        let resolved = validate_relative_file(&self.manifest.id, &self.package_dir, &rel)?;
        let bytes = read_file(&self.manifest.id, &rel, &resolved)?;
        let components: DesignPackageComponents =
            serde_json::from_slice(&bytes).map_err(|_| DesignRegistryError::ComponentsInvalid {
                id: self.manifest.id.clone(),
            })?;
        if components.schema != DESIGN_PACKAGE_COMPONENTS_SCHEMA
            || components.design_system_id != self.manifest.id
        {
            return Err(DesignRegistryError::ComponentsInvalid {
                id: self.manifest.id.clone(),
            });
        }
        Ok(Some(components))
    }
}

/// A registry over an ordered list of search roots.
#[derive(Debug, Clone)]
pub struct DesignRegistry {
    /// (provenance, root directory) in search-order precedence. Earlier entries
    /// win when the same id exists in multiple roots.
    roots: Vec<(PackageProvenance, PathBuf)>,
}

impl DesignRegistry {
    /// Build a registry with the canonical search order: local workspace first,
    /// then the shared location. `workspace_root` is the project root; the local
    /// design-systems directory is `<workspace>/.opensks/design-systems`.
    pub fn with_default_order(workspace_root: &Path, shared_root: Option<&Path>) -> Self {
        let mut roots = vec![(
            PackageProvenance::Local,
            workspace_root.join(".opensks").join("design-systems"),
        )];
        if let Some(shared) = shared_root {
            roots.push((PackageProvenance::Shared, shared.to_path_buf()));
        }
        Self { roots }
    }

    /// Build a registry from an explicit ordered root list (provenance, dir).
    /// Earlier roots take precedence.
    pub fn from_roots(roots: Vec<(PackageProvenance, PathBuf)>) -> Self {
        Self { roots }
    }

    /// The configured search roots, in precedence order.
    pub fn roots(&self) -> &[(PackageProvenance, PathBuf)] {
        &self.roots
    }

    /// Resolve a package by id, returning the first matching root in search
    /// order after full strict validation. A package dir that exists but fails
    /// validation surfaces the validation error (it does not silently fall
    /// through to a lower-precedence root).
    pub fn resolve(&self, id: &str) -> Result<ResolvedPackage, DesignRegistryError> {
        if !is_valid_package_id(id) {
            return Err(DesignRegistryError::InvalidPackageId { id: id.to_string() });
        }
        for (provenance, root) in &self.roots {
            let package_dir = root.join(id);
            let manifest_path = package_dir.join(MANIFEST_FILE_NAME);
            if !manifest_path.exists() {
                continue;
            }
            let manifest = validate_package(id, &package_dir)?;
            return Ok(ResolvedPackage {
                manifest,
                package_dir,
                provenance: *provenance,
            });
        }
        Err(DesignRegistryError::PackageNotFound { id: id.to_string() })
    }

    /// List the ids discoverable in any root (deduplicated, search-order
    /// precedence). Does not validate; useful for enumeration UIs.
    pub fn list_ids(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for (_provenance, root) in &self.roots {
            let Ok(entries) = fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
                    continue;
                };
                if !is_valid_package_id(&name) {
                    continue;
                }
                if entry.path().join(MANIFEST_FILE_NAME).exists() && !seen.contains(&name) {
                    seen.push(name);
                }
            }
        }
        seen
    }
}

/// Validate a package directory and return its parsed manifest.
///
/// Enforces, in order: manifest parses and its schema marker matches; the id in
/// the manifest matches the directory id; the license is present and non-empty;
/// every referenced file (design/tokens/components/usage) is package-relative,
/// symlink-free, and canonically contained; and every declared content hash
/// matches the on-disk bytes.
pub fn validate_package(
    id: &str,
    package_dir: &Path,
) -> Result<DesignPackageManifest, DesignRegistryError> {
    let canonical_dir = package_dir
        .canonicalize()
        .map_err(|_| DesignRegistryError::PackageNotFound { id: id.to_string() })?;

    let manifest_path = canonical_dir.join(MANIFEST_FILE_NAME);
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|_| DesignRegistryError::ManifestUnreadable { id: id.to_string() })?;
    let manifest: DesignPackageManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| DesignRegistryError::ManifestInvalid { id: id.to_string() })?;

    if manifest.schema != DESIGN_PACKAGE_MANIFEST_SCHEMA {
        return Err(DesignRegistryError::ManifestSchemaMismatch {
            id: id.to_string(),
            found: manifest.schema.clone(),
        });
    }
    if manifest.id != id {
        return Err(DesignRegistryError::ManifestIdMismatch {
            id: id.to_string(),
            manifest_id: manifest.id.clone(),
        });
    }
    if manifest.license.trim().is_empty() {
        return Err(DesignRegistryError::MissingLicense { id: id.to_string() });
    }

    // Every referenced path must be package-relative, symlink-free, contained.
    let mut referenced: Vec<String> =
        vec![manifest.files.design.clone(), manifest.files.tokens.clone()];
    if let Some(components) = &manifest.files.components {
        referenced.push(components.clone());
    }
    if let Some(usage) = &manifest.files.usage {
        referenced.push(usage.clone());
    }
    for rel in &referenced {
        validate_relative_file(id, &canonical_dir, rel)?;
    }

    // Declared content hashes must match the on-disk bytes (and point at files
    // that themselves pass path validation).
    let mut verified: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for entry in &manifest.content_hashes {
        let resolved = validate_relative_file(id, &canonical_dir, &entry.path)?;
        let bytes = read_file(id, &entry.path, &resolved)?;
        let actual = content_hash(&bytes);
        if actual != entry.hash {
            return Err(DesignRegistryError::ContentHashMismatch {
                id: id.to_string(),
                path: entry.path.clone(),
                declared: entry.hash.clone(),
                actual,
            });
        }
        verified.insert(entry.path.as_str());
    }

    // Integrity is mandatory, not opt-in: EVERY referenced file must carry a
    // declared-and-verified content hash. This rejects a manifest that omits
    // `content_hashes` entirely, hashes only a subset of the referenced files,
    // or declares hashes only for decoy (unreferenced) paths — any of which
    // would otherwise let a tampered DESIGN/tokens/components file validate.
    for rel in &referenced {
        if !verified.contains(rel.as_str()) {
            return Err(DesignRegistryError::MissingContentHash {
                id: id.to_string(),
                path: rel.clone(),
            });
        }
    }

    Ok(manifest)
}

/// Resolve a package-relative path under `package_dir`, enforcing the
/// file-service hardening order: reject absolute paths and `..` (syntactic),
/// reject symlinks at the target or any intermediate component, then verify the
/// canonicalized path is contained within the (already canonical) package dir.
/// Returns the resolved (non-canonical) absolute path on success.
pub fn validate_relative_file(
    id: &str,
    package_dir: &Path,
    relative: &str,
) -> Result<PathBuf, DesignRegistryError> {
    // 1. Syntactic: package-relative only; no absolute, no `..`, no root/prefix.
    if relative.is_empty() {
        return Err(DesignRegistryError::PathInvalid {
            id: id.to_string(),
            path: relative.to_string(),
        });
    }
    let rel = Path::new(relative);
    if rel.is_absolute() {
        return Err(DesignRegistryError::PathEscape {
            id: id.to_string(),
            path: relative.to_string(),
        });
    }
    let mut normalized = PathBuf::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(DesignRegistryError::PathEscape {
                    id: id.to_string(),
                    path: relative.to_string(),
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(DesignRegistryError::PathInvalid {
            id: id.to_string(),
            path: relative.to_string(),
        });
    }
    let candidate = package_dir.join(&normalized);

    // 2. Symlink rejection: walk each existing component, never following links.
    guard_no_symlink(id, package_dir, &candidate, relative)?;

    // 3. Canonical containment: the deepest existing ancestor must canonicalize
    //    inside the package dir.
    let existing = deepest_existing_ancestor(&candidate);
    let canonical_existing =
        existing
            .canonicalize()
            .map_err(|_| DesignRegistryError::FileNotFound {
                id: id.to_string(),
                path: relative.to_string(),
            })?;
    if !canonical_existing.starts_with(package_dir) {
        return Err(DesignRegistryError::PathEscape {
            id: id.to_string(),
            path: relative.to_string(),
        });
    }
    Ok(candidate)
}

/// Reject if the target itself or any intermediate component is a symlink.
fn guard_no_symlink(
    id: &str,
    package_dir: &Path,
    resolved: &Path,
    relative: &str,
) -> Result<(), DesignRegistryError> {
    let mut current = package_dir.to_path_buf();
    let suffix = resolved.strip_prefix(package_dir).unwrap_or(resolved);
    for component in suffix.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(DesignRegistryError::SymlinkRejected {
                        id: id.to_string(),
                        path: relative.to_string(),
                    });
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    Ok(())
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

fn read_file(id: &str, relative: &str, resolved: &Path) -> Result<Vec<u8>, DesignRegistryError> {
    fs::read(resolved).map_err(|_| DesignRegistryError::FileNotFound {
        id: id.to_string(),
        path: relative.to_string(),
    })
}

/// Stable FNV-1a content hash matching the repo's `fnv1a64:` convention.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

/// A package id must be a single safe path segment: no separators, no `.`/`..`,
/// no whitespace. This keeps `root.join(id)` from escaping the search root.
pub fn is_valid_package_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A normalized legacy `DESIGN.md`: a stable title plus an ordered section map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedDesign {
    /// The H1 title (first `# ` heading), or a default when absent.
    pub title: String,
    /// `(section heading, body)` pairs in document order, from `## ` headings.
    pub sections: Vec<(String, String)>,
    /// The raw markdown, preserved verbatim for the package `DESIGN` field.
    pub raw_markdown: String,
}

/// Normalize a legacy free-form `DESIGN.md` into a [`NormalizedDesign`].
///
/// The legacy format is just markdown with a single H1 title and `##` sections.
/// Normalization extracts the title and section bodies into a stable shape while
/// preserving the original markdown verbatim, so a package can carry both the
/// machine-readable map and the human-authored prose.
pub fn normalize_legacy_design(markdown: &str) -> NormalizedDesign {
    let mut title = String::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_body = String::new();

    for line in markdown.lines() {
        if let Some(h1) = line.strip_prefix("# ") {
            if title.is_empty() {
                title = h1.trim().to_string();
            }
            continue;
        }
        if let Some(h2) = line.strip_prefix("## ") {
            if let Some(heading) = current_heading.take() {
                sections.push((heading, current_body.trim().to_string()));
                current_body.clear();
            }
            current_heading = Some(h2.trim().to_string());
            continue;
        }
        if current_heading.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    if let Some(heading) = current_heading.take() {
        sections.push((heading, current_body.trim().to_string()));
    }

    NormalizedDesign {
        title: if title.is_empty() {
            "Untitled Design".to_string()
        } else {
            title
        },
        sections,
        raw_markdown: markdown.to_string(),
    }
}

/// Validation / discovery error taxonomy. Content-free: variants carry only
/// package ids, package-relative paths, and stable reason codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesignRegistryError {
    InvalidPackageId {
        id: String,
    },
    PackageNotFound {
        id: String,
    },
    ManifestUnreadable {
        id: String,
    },
    ManifestInvalid {
        id: String,
    },
    ManifestSchemaMismatch {
        id: String,
        found: String,
    },
    ManifestIdMismatch {
        id: String,
        manifest_id: String,
    },
    MissingLicense {
        id: String,
    },
    PathInvalid {
        id: String,
        path: String,
    },
    PathEscape {
        id: String,
        path: String,
    },
    SymlinkRejected {
        id: String,
        path: String,
    },
    FileNotFound {
        id: String,
        path: String,
    },
    ContentHashMismatch {
        id: String,
        path: String,
        declared: String,
        actual: String,
    },
    /// A referenced file (design/tokens/components/usage) has no declared,
    /// verified content hash. An omitted, partial, or decoy-only
    /// `content_hashes` set must not pass validation.
    MissingContentHash {
        id: String,
        path: String,
    },
    TokensInvalid {
        id: String,
    },
    TokensIdMismatch {
        id: String,
        tokens_id: String,
    },
    ComponentsInvalid {
        id: String,
    },
}

impl DesignRegistryError {
    /// Stable machine-readable reason code.
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::InvalidPackageId { .. } => "design_package_id_invalid",
            Self::PackageNotFound { .. } => "design_package_not_found",
            Self::ManifestUnreadable { .. } => "design_manifest_unreadable",
            Self::ManifestInvalid { .. } => "design_manifest_invalid",
            Self::ManifestSchemaMismatch { .. } => "design_manifest_schema_mismatch",
            Self::ManifestIdMismatch { .. } => "design_manifest_id_mismatch",
            Self::MissingLicense { .. } => "design_manifest_missing_license",
            Self::PathInvalid { .. } => "design_path_invalid",
            Self::PathEscape { .. } => "design_path_escape",
            Self::SymlinkRejected { .. } => "design_symlink_rejected",
            Self::FileNotFound { .. } => "design_file_not_found",
            Self::ContentHashMismatch { .. } => "design_content_hash_mismatch",
            Self::MissingContentHash { .. } => "design_missing_content_hash",
            Self::TokensInvalid { .. } => "design_tokens_invalid",
            Self::TokensIdMismatch { .. } => "design_tokens_id_mismatch",
            Self::ComponentsInvalid { .. } => "design_components_invalid",
        }
    }
}

impl std::fmt::Display for DesignRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContentHashMismatch {
                id,
                path,
                declared,
                actual,
            } => write!(
                f,
                "{}: package {id} file {path} declared {declared} but found {actual}",
                self.reason_code()
            ),
            Self::ManifestSchemaMismatch { id, found } => write!(
                f,
                "{}: package {id} manifest schema is {found}",
                self.reason_code()
            ),
            Self::ManifestIdMismatch { id, manifest_id } => write!(
                f,
                "{}: directory {id} manifest declares id {manifest_id}",
                self.reason_code()
            ),
            Self::TokensIdMismatch { id, tokens_id } => write!(
                f,
                "{}: package {id} tokens bind to {tokens_id}",
                self.reason_code()
            ),
            Self::PathInvalid { id, path }
            | Self::PathEscape { id, path }
            | Self::SymlinkRejected { id, path }
            | Self::FileNotFound { id, path }
            | Self::MissingContentHash { id, path } => {
                write!(f, "{}: package {id} path {path}", self.reason_code())
            }
            other => write!(f, "{}", other.reason_code()),
        }
    }
}

impl std::error::Error for DesignRegistryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

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
                "opensks-design-registry-{name}-{}-{stamp}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("create temp dir");
            Self {
                root: root.canonicalize().expect("canonicalize temp dir"),
            }
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

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Write a minimal valid package under `<root>/design-systems/<id>/`.
    fn write_valid_package(dir: &TempDir, root_label: &str, id: &str, license: &str) {
        let base = format!("{root_label}/{id}");
        let tokens = format!(
            "{{\"schema\":\"opensks.design-token-set.v1\",\"design_system_id\":\"{id}\",\"revision\":1,\"tokens\":[{{\"path\":\"color.canvas\",\"type\":\"color\",\"value\":\"#000000\"}}]}}"
        );
        let design = "# Title\n\n## Section\n\nBody\n";
        dir.write(&format!("{base}/tokens.json"), &tokens);
        dir.write(&format!("{base}/DESIGN.md"), design);
        let tokens_hash = content_hash(tokens.as_bytes());
        let design_hash = content_hash(design.as_bytes());
        let manifest = format!(
            "{{\"schema\":\"opensks.design-package.v1\",\"id\":\"{id}\",\"name\":\"{id}\",\"version\":\"1.0.0\",\"license\":\"{license}\",\"description\":\"d\",\"package_schema_version\":1,\"files\":{{\"design\":\"DESIGN.md\",\"tokens\":\"tokens.json\"}},\"content_hashes\":[{{\"path\":\"tokens.json\",\"hash\":\"{tokens_hash}\"}},{{\"path\":\"DESIGN.md\",\"hash\":\"{design_hash}\"}}],\"platforms\":[\"macos-swiftui\"]}}"
        );
        dir.write(&format!("{base}/manifest.json"), &manifest);
    }

    #[test]
    fn valid_package_validates_and_loads_tokens() {
        let dir = TempDir::new("valid");
        write_valid_package(&dir, "local", "demo", "MIT");
        let pkg_dir = dir.path("local/demo");
        let manifest = validate_package("demo", &pkg_dir).expect("validates");
        assert_eq!(manifest.license, "MIT");

        let registry =
            DesignRegistry::from_roots(vec![(PackageProvenance::Local, dir.path("local"))]);
        let resolved = registry.resolve("demo").expect("resolve");
        assert_eq!(resolved.provenance, PackageProvenance::Local);
        let tokens = resolved.load_tokens().expect("load tokens");
        assert_eq!(tokens.design_system_id, "demo");
    }

    #[test]
    fn absolute_path_in_manifest_is_rejected() {
        let dir = TempDir::new("abs");
        write_valid_package(&dir, "local", "demo", "MIT");
        // Repoint `design` at an absolute path.
        let manifest = "{\"schema\":\"opensks.design-package.v1\",\"id\":\"demo\",\"name\":\"demo\",\"version\":\"1.0.0\",\"license\":\"MIT\",\"description\":\"d\",\"package_schema_version\":1,\"files\":{\"design\":\"/etc/passwd\",\"tokens\":\"tokens.json\"}}";
        dir.write("local/demo/manifest.json", manifest);
        let error = validate_package("demo", &dir.path("local/demo")).expect_err("absolute");
        assert_eq!(error.reason_code(), "design_path_escape");
    }

    #[test]
    fn parent_traversal_in_manifest_is_rejected() {
        let dir = TempDir::new("traversal");
        write_valid_package(&dir, "local", "demo", "MIT");
        // Put a secret file outside the package, then traverse to it.
        dir.write("local/secret.txt", "outside\n");
        let manifest = "{\"schema\":\"opensks.design-package.v1\",\"id\":\"demo\",\"name\":\"demo\",\"version\":\"1.0.0\",\"license\":\"MIT\",\"description\":\"d\",\"package_schema_version\":1,\"files\":{\"design\":\"../secret.txt\",\"tokens\":\"tokens.json\"}}";
        dir.write("local/demo/manifest.json", manifest);
        let error = validate_package("demo", &dir.path("local/demo")).expect_err("traversal");
        assert_eq!(error.reason_code(), "design_path_escape");
    }

    #[test]
    fn symlink_pointing_outside_package_is_rejected() {
        let dir = TempDir::new("symlink");
        write_valid_package(&dir, "local", "demo", "MIT");
        // A real outside target so the link is not dangling.
        dir.write("outside-design.md", "# outside\n");
        let link = dir.path("local/demo/DESIGN.md");
        fs::remove_file(&link).expect("remove regular DESIGN.md");
        symlink(dir.path("outside-design.md"), &link).expect("create symlink");

        let error = validate_package("demo", &dir.path("local/demo")).expect_err("symlink");
        assert_eq!(error.reason_code(), "design_symlink_rejected");
    }

    #[test]
    fn declared_but_mismatched_content_hash_is_rejected() {
        let dir = TempDir::new("hash");
        write_valid_package(&dir, "local", "demo", "MIT");
        // Tamper with tokens.json after the manifest hash was computed.
        dir.write(
            "local/demo/tokens.json",
            "{\"schema\":\"opensks.design-token-set.v1\",\"design_system_id\":\"demo\",\"revision\":2,\"tokens\":[{\"path\":\"color.canvas\",\"type\":\"color\",\"value\":\"#111111\"}]}",
        );
        let error = validate_package("demo", &dir.path("local/demo")).expect_err("hash mismatch");
        assert_eq!(error.reason_code(), "design_content_hash_mismatch");
    }

    #[test]
    fn referenced_file_without_a_verified_content_hash_is_rejected() {
        // Integrity must be mandatory: omitting, partially declaring, or
        // declaring only decoy content hashes must all fail closed, otherwise a
        // tampered DESIGN.md / tokens.json would validate (CVE-class bypass).
        let base = "{\"schema\":\"opensks.design-package.v1\",\"id\":\"demo\",\"name\":\"demo\",\"version\":\"1.0.0\",\"license\":\"MIT\",\"description\":\"d\",\"package_schema_version\":1,\"files\":{\"design\":\"DESIGN.md\",\"tokens\":\"tokens.json\"}";

        // (a) content_hashes omitted entirely.
        let dir = TempDir::new("nohash-omit");
        write_valid_package(&dir, "local", "demo", "MIT");
        dir.write("local/demo/manifest.json", &format!("{base}}}"));
        let error = validate_package("demo", &dir.path("local/demo")).expect_err("omit");
        assert_eq!(error.reason_code(), "design_missing_content_hash");

        // (b) partial: a valid hash for tokens.json only; DESIGN.md unverified.
        let dir = TempDir::new("nohash-partial");
        write_valid_package(&dir, "local", "demo", "MIT");
        let tokens = fs::read(dir.path("local/demo/tokens.json")).expect("read tokens");
        let th = content_hash(&tokens);
        dir.write(
            "local/demo/manifest.json",
            &format!(
                "{base},\"content_hashes\":[{{\"path\":\"tokens.json\",\"hash\":\"{th}\"}}]}}"
            ),
        );
        let error = validate_package("demo", &dir.path("local/demo")).expect_err("partial");
        assert_eq!(error.reason_code(), "design_missing_content_hash");

        // (c) decoy: a hash only for an unreferenced file; referenced files unverified.
        let dir = TempDir::new("nohash-decoy");
        write_valid_package(&dir, "local", "demo", "MIT");
        dir.write("local/demo/decoy.txt", "decoy\n");
        let dh = content_hash(b"decoy\n");
        dir.write(
            "local/demo/manifest.json",
            &format!("{base},\"content_hashes\":[{{\"path\":\"decoy.txt\",\"hash\":\"{dh}\"}}]}}"),
        );
        let error = validate_package("demo", &dir.path("local/demo")).expect_err("decoy");
        assert_eq!(error.reason_code(), "design_missing_content_hash");
    }

    #[test]
    fn missing_license_is_rejected() {
        let dir = TempDir::new("license");
        write_valid_package(&dir, "local", "demo", "");
        let error = validate_package("demo", &dir.path("local/demo")).expect_err("license");
        assert_eq!(error.reason_code(), "design_manifest_missing_license");
    }

    #[test]
    fn search_order_resolves_local_over_shared_with_provenance() {
        let dir = TempDir::new("order");
        // Same id exists in both roots; local must win.
        write_valid_package(&dir, "local", "shared-demo", "MIT");
        write_valid_package(&dir, "shared", "shared-demo", "Apache-2.0");
        // And a shared-only id resolves with Shared provenance.
        write_valid_package(&dir, "shared", "shared-only", "MIT");

        let registry = DesignRegistry::from_roots(vec![
            (PackageProvenance::Local, dir.path("local")),
            (PackageProvenance::Shared, dir.path("shared")),
        ]);

        let resolved = registry.resolve("shared-demo").expect("resolve");
        assert_eq!(resolved.provenance, PackageProvenance::Local);
        assert_eq!(resolved.manifest.license, "MIT");

        let shared = registry.resolve("shared-only").expect("resolve shared");
        assert_eq!(shared.provenance, PackageProvenance::Shared);

        let ids = registry.list_ids();
        assert!(ids.contains(&"shared-demo".to_string()));
        assert!(ids.contains(&"shared-only".to_string()));
        // shared-demo deduped to a single entry despite being in both roots.
        assert_eq!(ids.iter().filter(|id| *id == "shared-demo").count(), 1);
    }

    #[test]
    fn default_order_puts_local_workspace_first() {
        let dir = TempDir::new("default-order");
        let shared = dir.path("shared-loc");
        let registry = DesignRegistry::with_default_order(&dir.root, Some(&shared));
        let roots = registry.roots();
        assert_eq!(roots[0].0, PackageProvenance::Local);
        assert!(roots[0].1.ends_with(".opensks/design-systems"));
        assert_eq!(roots[1].0, PackageProvenance::Shared);
    }

    #[test]
    fn invalid_package_id_is_rejected_before_filesystem() {
        let dir = TempDir::new("bad-id");
        let registry =
            DesignRegistry::from_roots(vec![(PackageProvenance::Local, dir.path("local"))]);
        let error = registry.resolve("../escape").expect_err("bad id");
        assert_eq!(error.reason_code(), "design_package_id_invalid");
    }

    #[test]
    fn legacy_design_md_normalizes_title_and_sections() {
        let legacy = "# OpenSKS Studio Dark — Design Guide\n\nIntro line.\n\n## Visual Theme\n\nA calm dark workspace.\n\n## Typography Rules\n\n7:1 contrast.\n";
        let normalized = normalize_legacy_design(legacy);
        assert_eq!(normalized.title, "OpenSKS Studio Dark — Design Guide");
        assert_eq!(normalized.sections.len(), 2);
        assert_eq!(normalized.sections[0].0, "Visual Theme");
        assert!(normalized.sections[0].1.contains("calm dark workspace"));
        assert_eq!(normalized.sections[1].0, "Typography Rules");
        // Raw markdown preserved verbatim.
        assert_eq!(normalized.raw_markdown, legacy);
    }

    #[test]
    fn legacy_design_md_without_title_uses_default() {
        let normalized = normalize_legacy_design("## Only A Section\n\nBody.\n");
        assert_eq!(normalized.title, "Untitled Design");
        assert_eq!(normalized.sections.len(), 1);
    }
}
