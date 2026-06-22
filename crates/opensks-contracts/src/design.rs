//! Portable design-PACKAGE contracts (`opensks.design-package.v1`).
//!
//! PR-037 lifts the bootstrap `opensks.design-project.v1` manifest (which lives
//! in the `opensks-design` crate) into the shared contracts crate as the
//! portable *package* format: a self-contained, hashable, license-bearing bundle
//! that a registry can discover, validate, and resolve by id. The wire shapes
//! live here so the daemon, the editor, and the design registry share a single
//! source of truth; the discovery + strict path/hash/license validation engine
//! lives in `opensks-design`.
//!
//! A package is a directory containing a `manifest.json` plus the files it
//! references (a prose `DESIGN` document, a `tokens` document, and an optional
//! `components` catalog). Every referenced path is package-relative; the
//! registry rejects absolute paths, `..` traversal, and symlinks, and verifies
//! the declared content hashes against the on-disk bytes before a package is
//! considered trusted.
//!
//! Invariant: a manifest never embeds machine-absolute paths or secret values.
//! `content_hashes` carry only stable `fnv1a64:` digests, never file contents.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `schema` value for a portable design-package manifest.
pub const DESIGN_PACKAGE_MANIFEST_SCHEMA: &str = "opensks.design-package.v1";
/// `schema` value for a design-package token document.
pub const DESIGN_PACKAGE_TOKENS_SCHEMA: &str = "opensks.design-token-set.v1";
/// `schema` value for a design-package component catalog.
pub const DESIGN_PACKAGE_COMPONENTS_SCHEMA: &str = "opensks.component-catalog.v1";

/// Where a package's tokens/components/design files are found, relative to the
/// package directory. Absolute paths and `..` are contract violations the
/// registry rejects; the shapes only ever hold package-relative strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DesignPackageFiles {
    /// Package-relative path to the prose `DESIGN` document (e.g. `DESIGN.md`).
    pub design: String,
    /// Package-relative path to the token document (e.g. `tokens.json`).
    pub tokens: String,
    /// Package-relative path to the optional component catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<String>,
    /// Package-relative path to an optional usage / examples document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
}

/// A single declared content hash binding a package-relative file to a stable
/// digest. The registry recomputes the digest from the on-disk bytes and
/// rejects the package on any mismatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DesignContentHash {
    /// Package-relative path the hash covers.
    pub path: String,
    /// Stable digest of the file bytes (`fnv1a64:<hex>`).
    pub hash: String,
}

/// Provenance of an imported package, redacted of any userinfo/secret material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DesignPackageSource {
    /// `project`, `import`, `vendor`, etc.
    pub kind: String,
    /// Redacted origin reference (never embeds credentials).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_at: Option<String>,
}

/// Declared security posture of a package's assets. A package that claims it
/// carries no executable code or remote URLs is held to that claim by importers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct DesignPackageSecurity {
    #[serde(default)]
    pub contains_executable_code: bool,
    #[serde(default)]
    pub contains_remote_urls: bool,
    #[serde(default)]
    pub allowed_asset_media_types: Vec<String>,
}

/// A portable design-system package manifest (`manifest.json`).
///
/// The manifest is the discovery + trust root: the registry reads it, resolves
/// every `files` path under the package directory, and verifies each declared
/// `content_hashes` entry. `license` is mandatory — a package with no license
/// cannot be trusted or redistributed, so the registry rejects an empty one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DesignPackageManifest {
    /// Must equal [`DESIGN_PACKAGE_MANIFEST_SCHEMA`].
    pub schema: String,
    /// Stable package identifier (the registry resolves packages by this id).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Semantic version string of the package contents.
    pub version: String,
    /// SPDX-style license identifier. Mandatory and non-empty.
    pub license: String,
    /// One-line description of the package's intent.
    pub description: String,
    /// Schema/format revision of the package layout itself (monotonic).
    pub package_schema_version: u32,
    /// Where the design / tokens / components documents live, package-relative.
    pub files: DesignPackageFiles,
    /// Declared content hashes the registry verifies against the on-disk files.
    #[serde(default)]
    pub content_hashes: Vec<DesignContentHash>,
    /// Target platforms this package can compile adapters for.
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<DesignPackageSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<DesignPackageSecurity>,
}

impl DesignPackageManifest {
    /// Look up the declared content hash for a package-relative path.
    pub fn declared_hash(&self, path: &str) -> Option<&str> {
        self.content_hashes
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.hash.as_str())
    }
}

/// A single semantic token in a package token document. Mirrors the platform-
/// neutral IR that `opensks-design` compiles into platform adapters: `value` is
/// a JSON scalar so colors (hex string) and dimensions (number) share one type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DesignPackageToken {
    pub path: String,
    #[serde(rename = "type")]
    pub token_type: String,
    pub value: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub contrast_constraints: Vec<serde_json::Value>,
}

/// A package token document (`tokens.json`). Bound to a manifest by
/// `design_system_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DesignPackageTokens {
    /// Must equal [`DESIGN_PACKAGE_TOKENS_SCHEMA`].
    pub schema: String,
    pub design_system_id: String,
    pub revision: u32,
    #[serde(default)]
    pub tokens: Vec<DesignPackageToken>,
}

/// A single component entry in a package component catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DesignPackageComponent {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub token_refs: Vec<String>,
}

/// A package component catalog (`components.json`). Bound to a manifest by
/// `design_system_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DesignPackageComponents {
    /// Must equal [`DESIGN_PACKAGE_COMPONENTS_SCHEMA`].
    pub schema: String,
    pub design_system_id: String,
    #[serde(default)]
    pub components: Vec<DesignPackageComponent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrips_with_license_and_hashes() {
        let manifest = DesignPackageManifest {
            schema: DESIGN_PACKAGE_MANIFEST_SCHEMA.to_string(),
            id: "demo".to_string(),
            name: "Demo".to_string(),
            version: "1.0.0".to_string(),
            license: "MIT".to_string(),
            description: "demo package".to_string(),
            package_schema_version: 1,
            files: DesignPackageFiles {
                design: "DESIGN.md".to_string(),
                tokens: "tokens.json".to_string(),
                components: Some("components.json".to_string()),
                usage: None,
            },
            content_hashes: vec![DesignContentHash {
                path: "tokens.json".to_string(),
                hash: "fnv1a64:0000000000000000".to_string(),
            }],
            platforms: vec!["macos-swiftui".to_string()],
            source: None,
            security: None,
        };
        let json = serde_json::to_string(&manifest).expect("serialize manifest");
        assert!(json.contains("\"schema\":\"opensks.design-package.v1\""));
        assert!(json.contains("\"license\":\"MIT\""));
        let decoded: DesignPackageManifest = serde_json::from_str(&json).expect("decode manifest");
        assert_eq!(decoded, manifest);
        assert_eq!(
            decoded.declared_hash("tokens.json"),
            Some("fnv1a64:0000000000000000")
        );
        assert_eq!(decoded.declared_hash("missing.json"), None);
    }

    #[test]
    fn tokens_and_components_bind_by_design_system_id() {
        let tokens = DesignPackageTokens {
            schema: DESIGN_PACKAGE_TOKENS_SCHEMA.to_string(),
            design_system_id: "demo".to_string(),
            revision: 1,
            tokens: vec![DesignPackageToken {
                path: "color.canvas".to_string(),
                token_type: "color".to_string(),
                value: serde_json::json!("#000000"),
                unit: None,
                semantic_role: Some("application-background".to_string()),
                confidence: Some("curated".to_string()),
                source_refs: vec![],
                contrast_constraints: vec![],
            }],
        };
        let components = DesignPackageComponents {
            schema: DESIGN_PACKAGE_COMPONENTS_SCHEMA.to_string(),
            design_system_id: "demo".to_string(),
            components: vec![],
        };
        assert_eq!(tokens.design_system_id, components.design_system_id);
        let token_json = serde_json::to_string(&tokens).expect("tokens json");
        assert!(token_json.contains("\"type\":\"color\""));
    }
}
