//! Token-draft persistence + isolated compile + registry listing for the Design
//! Studio (recovery release §16.3 — DESIGN-002 / DESIGN-101).
//!
//! The crate already owns discovery (registry), validation (compile/audit), and
//! atomic activation. This module adds the editorial layer the Studio needs:
//!
//! * [`save_token_values`] — persist edited token *values* (by path) back into a
//!   package's `tokens.json`, atomically (temp + rename). Only existing paths are
//!   updated; unknown paths are reported, never silently created.
//! * [`compile_package`] — compile/validate a package's tokens in isolation (no
//!   activation), for editorial feedback, reusing the strict compiler.
//! * [`list_packages`] — enumerate the registry for the Studio catalog
//!   (replaces the hard-coded Swift seed).

use std::fs;
use std::path::{Path, PathBuf};

use crate::activation::read_active;
use crate::compile_swift_tokens_checked;
use crate::contracts::DesignTokenSet;
use crate::registry::{DesignRegistry, content_hash};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftError {
    NotFound(String),
    Io(String),
    Parse(String),
}

impl std::fmt::Display for DraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DraftError::NotFound(m) => write!(f, "design token set not found: {m}"),
            DraftError::Io(m) => write!(f, "design draft io error: {m}"),
            DraftError::Parse(m) => write!(f, "design token set parse error: {m}"),
        }
    }
}

impl std::error::Error for DraftError {}

/// `<workspace>/.opensks/design-systems/<id>/tokens.json` — the local package's
/// token document (the editable source).
fn tokens_path(workspace: &Path, package_id: &str) -> PathBuf {
    workspace
        .join(".opensks")
        .join("design-systems")
        .join(package_id)
        .join("tokens.json")
}

fn load_set(workspace: &Path, package_id: &str) -> Result<DesignTokenSet, DraftError> {
    let path = tokens_path(workspace, package_id);
    let raw = fs::read_to_string(&path)
        .map_err(|e| DraftError::NotFound(format!("{}: {e}", path.display())))?;
    serde_json::from_str(&raw).map_err(|e| DraftError::Parse(e.to_string()))
}

/// The result of persisting a token draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveTokensOutcome {
    pub package_id: String,
    pub updated: usize,
    pub unknown_paths: Vec<String>,
    pub total: usize,
    pub content_hash: String,
}

/// Persist edited token values (by path) into the package's `tokens.json`,
/// atomically. Existing tokens are updated in place (value kind preserved);
/// unknown paths are returned, never created.
pub fn save_token_values(
    workspace: &Path,
    package_id: &str,
    updates: &[(String, String)],
) -> Result<SaveTokensOutcome, DraftError> {
    let mut set = load_set(workspace, package_id)?;
    let mut updated = 0usize;
    let mut unknown = Vec::new();
    for (path, value) in updates {
        match set.tokens.iter_mut().find(|t| &t.path == path) {
            Some(token) => {
                token.value = coerce_value(&token.value, value);
                updated += 1;
            }
            None => unknown.push(path.clone()),
        }
    }
    let serialized =
        serde_json::to_string_pretty(&set).map_err(|e| DraftError::Parse(e.to_string()))? + "\n";
    write_atomic(&tokens_path(workspace, package_id), serialized.as_bytes())?;
    Ok(SaveTokensOutcome {
        package_id: package_id.to_string(),
        updated,
        unknown_paths: unknown,
        total: set.tokens.len(),
        content_hash: content_hash(serialized.as_bytes()),
    })
}

/// Coerce an edited string into the JSON kind the existing value used, so a
/// dimension token (number) stays a number and a color (string) stays a string.
fn coerce_value(existing: &serde_json::Value, raw: &str) -> serde_json::Value {
    if existing.is_number() {
        if let Ok(i) = raw.parse::<i64>() {
            return serde_json::Value::from(i);
        }
        if let Ok(f) = raw.parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(f) {
                return serde_json::Value::Number(n);
            }
        }
    }
    if existing.is_boolean() {
        if let Ok(b) = raw.parse::<bool>() {
            return serde_json::Value::Bool(b);
        }
    }
    serde_json::Value::String(raw.to_string())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), DraftError> {
    let dir = path
        .parent()
        .ok_or_else(|| DraftError::Io("token path has no parent directory".to_string()))?;
    fs::create_dir_all(dir).map_err(|e| DraftError::Io(e.to_string()))?;
    let tmp = dir.join(format!(".tokens.{}.tmp", std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| DraftError::Io(e.to_string()))?;
    // rename(2) is atomic within a directory: readers see old-or-new, never torn.
    fs::rename(&tmp, path).map_err(|e| DraftError::Io(e.to_string()))?;
    Ok(())
}

/// The result of compiling a package's tokens in isolation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileOutcome {
    pub package_id: String,
    pub ok: bool,
    pub swift_bytes: usize,
    pub error: Option<String>,
}

/// Compile + validate a package's tokens without activating it (editorial
/// feedback). Reuses the strict alias-resolving compiler.
pub fn compile_package(workspace: &Path, package_id: &str) -> Result<CompileOutcome, DraftError> {
    let set = load_set(workspace, package_id)?;
    Ok(match compile_swift_tokens_checked(&set) {
        Ok(swift) => CompileOutcome {
            package_id: package_id.to_string(),
            ok: true,
            swift_bytes: swift.len(),
            error: None,
        },
        Err(error) => CompileOutcome {
            package_id: package_id.to_string(),
            ok: false,
            swift_bytes: 0,
            error: Some(error.to_string()),
        },
    })
}

/// One package in the registry-driven catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSummary {
    pub package_id: String,
    pub title: String,
    pub active: bool,
}

/// Enumerate the registry's packages for the Design Studio catalog (DESIGN-101).
/// Resolution failures fall back to the id as the title rather than dropping the
/// package, so the catalog reflects what is on disk.
pub fn list_packages(workspace: &Path) -> Vec<PackageSummary> {
    let registry = DesignRegistry::with_default_order(workspace, None);
    let active = read_active(workspace).ok().and_then(|m| m.active_package);
    registry
        .list_ids()
        .into_iter()
        .map(|id| {
            let title = registry
                .resolve(&id)
                .map(|r| r.manifest.name)
                .unwrap_or_else(|_| id.clone());
            PackageSummary {
                active: active.as_deref() == Some(id.as_str()),
                package_id: id,
                title,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ws(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opensks-draft-{tag}-{}", std::process::id()));
        let pkg = dir.join(".opensks/design-systems/test-pkg");
        fs::create_dir_all(&pkg).unwrap();
        let tokens = r##"{
          "schema": "opensks.design-token-set.v1",
          "design_system_id": "test-pkg",
          "revision": 1,
          "tokens": [
            { "path": "color.canvas", "type": "color", "value": "#0E1015" },
            { "path": "size.hit_target.primary", "type": "dimension", "value": 44, "unit": "pt" }
          ]
        }"##;
        fs::write(pkg.join("tokens.json"), tokens).unwrap();
        fs::write(
            pkg.join("manifest.json"),
            r#"{"schema":"opensks.design-package.v1","id":"test-pkg","name":"Test Pkg","version":"1.0.0","license":"MIT","description":"t","package_schema_version":1,"files":{"design":"DESIGN.md","tokens":"tokens.json"},"platforms":["macos-swiftui"]}"#,
        )
        .unwrap();
        dir
    }

    #[test]
    fn save_persists_values_and_reports_unknown_paths() {
        let ws = temp_ws("save");
        let outcome = save_token_values(
            &ws,
            "test-pkg",
            &[
                ("color.canvas".to_string(), "#000000".to_string()),
                ("size.hit_target.primary".to_string(), "48".to_string()),
                ("color.nope".to_string(), "#fff".to_string()),
            ],
        )
        .unwrap();
        assert_eq!(outcome.updated, 2);
        assert_eq!(outcome.unknown_paths, vec!["color.nope".to_string()]);
        assert_eq!(outcome.total, 2);

        // Reload from disk: the edits persisted, value kinds preserved.
        let reloaded = load_set(&ws, "test-pkg").unwrap();
        let canvas = reloaded
            .tokens
            .iter()
            .find(|t| t.path == "color.canvas")
            .unwrap();
        assert_eq!(
            canvas.value,
            serde_json::Value::String("#000000".to_string())
        );
        let hit = reloaded
            .tokens
            .iter()
            .find(|t| t.path == "size.hit_target.primary")
            .unwrap();
        assert_eq!(hit.value, serde_json::json!(48)); // stayed a number
        fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn compile_validates_the_token_set() {
        let ws = temp_ws("compile");
        let outcome = compile_package(&ws, "test-pkg").unwrap();
        assert!(
            outcome.ok,
            "valid tokens should compile: {:?}",
            outcome.error
        );
        assert!(outcome.swift_bytes > 0);
        fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn list_enumerates_registry_packages() {
        let ws = temp_ws("list");
        let packages = list_packages(&ws);
        assert!(
            packages.iter().any(|p| p.package_id == "test-pkg"),
            "registry listing must include the on-disk package"
        );
        assert!(packages.iter().all(|p| !p.active)); // nothing activated yet
        fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn missing_package_is_a_typed_error() {
        let ws = temp_ws("missing");
        let err = save_token_values(&ws, "no-such-pkg", &[]).unwrap_err();
        assert!(matches!(err, DraftError::NotFound(_)));
        fs::remove_dir_all(&ws).ok();
    }
}
