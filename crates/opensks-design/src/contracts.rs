//! Minimal `opensks.design-project.v1` portable-package contracts.
//!
//! PR-021 bootstrap: just enough typed surface to validate the bundled
//! `opensks-studio-dark` package and compile its token set into platform
//! adapters. The full registry / compiler / audit engine arrives in PR-037+.

use serde::{Deserialize, Serialize};

/// `schema` value for a design-project manifest.
pub const DESIGN_PROJECT_SCHEMA: &str = "opensks.design-project.v1";
/// `schema` value for a design token set.
pub const DESIGN_TOKEN_SET_SCHEMA: &str = "opensks.design-token-set.v1";

/// A portable design-system package manifest (`manifest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignProjectManifest {
    pub schema: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub category: String,
    pub description: String,
    pub files: DesignProjectFiles,
    pub platforms: Vec<String>,
    #[serde(default)]
    pub source: Option<DesignSource>,
    #[serde(default)]
    pub security: Option<DesignSecurity>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignProjectFiles {
    pub design: String,
    pub tokens: String,
    #[serde(default)]
    pub components: Option<String>,
    #[serde(default)]
    pub usage: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignSource {
    pub kind: String,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub imported_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignSecurity {
    #[serde(default)]
    pub contains_executable_code: bool,
    #[serde(default)]
    pub contains_remote_urls: bool,
    #[serde(default)]
    pub allowed_asset_media_types: Vec<String>,
}

/// A platform-neutral semantic token set (`tokens.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignTokenSet {
    pub schema: String,
    pub design_system_id: String,
    pub revision: u32,
    pub tokens: Vec<DesignToken>,
}

/// A single semantic token. `value` is a JSON scalar so that colors (string
/// hex) and dimensions (number) share one type, exactly as the directive's
/// `opensks.design-token-set.v1` IR specifies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesignToken {
    pub path: String,
    #[serde(rename = "type")]
    pub token_type: String,
    pub value: serde_json::Value,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub semantic_role: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub source_refs: Vec<String>,
    #[serde(default)]
    pub contrast_constraints: Vec<serde_json::Value>,
}

/// Validation errors for a bootstrap package.
#[derive(Debug, Clone, PartialEq)]
pub enum DesignContractError {
    ManifestSchemaMismatch { found: String },
    TokenSetSchemaMismatch { found: String },
    TokenSetIdMismatch { manifest: String, tokens: String },
    EmptyTokenSet,
}

impl std::fmt::Display for DesignContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DesignContractError::ManifestSchemaMismatch { found } => {
                write!(
                    f,
                    "manifest schema must be {DESIGN_PROJECT_SCHEMA}, found {found}"
                )
            }
            DesignContractError::TokenSetSchemaMismatch { found } => {
                write!(
                    f,
                    "token set schema must be {DESIGN_TOKEN_SET_SCHEMA}, found {found}"
                )
            }
            DesignContractError::TokenSetIdMismatch { manifest, tokens } => {
                write!(
                    f,
                    "token set design_system_id {tokens} does not match manifest id {manifest}"
                )
            }
            DesignContractError::EmptyTokenSet => write!(f, "token set has no tokens"),
        }
    }
}

impl std::error::Error for DesignContractError {}

impl DesignProjectManifest {
    /// Validate the manifest's own schema marker.
    pub fn validate(&self) -> Result<(), DesignContractError> {
        if self.schema != DESIGN_PROJECT_SCHEMA {
            return Err(DesignContractError::ManifestSchemaMismatch {
                found: self.schema.clone(),
            });
        }
        Ok(())
    }
}

impl DesignTokenSet {
    /// Validate the token set's schema marker and that it binds to `manifest_id`.
    pub fn validate(&self, manifest_id: &str) -> Result<(), DesignContractError> {
        if self.schema != DESIGN_TOKEN_SET_SCHEMA {
            return Err(DesignContractError::TokenSetSchemaMismatch {
                found: self.schema.clone(),
            });
        }
        if self.design_system_id != manifest_id {
            return Err(DesignContractError::TokenSetIdMismatch {
                manifest: manifest_id.to_string(),
                tokens: self.design_system_id.clone(),
            });
        }
        if self.tokens.is_empty() {
            return Err(DesignContractError::EmptyTokenSet);
        }
        Ok(())
    }
}
