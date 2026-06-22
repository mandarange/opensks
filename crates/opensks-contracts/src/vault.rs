//! Portable summary + encrypted vault contracts (PR-042).
//!
//! These are the snake_case wire DTOs shared by the Rust vault implementation
//! and the Swift Studio app. The summary is sanitized and git-trackable; the
//! `.age` vault is ciphertext produced by the well-vetted `age` crate. No raw
//! transcript or secret material is ever serialized into the summary.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const VAULT_SUMMARY_SCHEMA: &str = "opensks.vault-summary.v1";
pub const VAULT_ENCRYPT_SCHEMA: &str = "opensks.vault-encrypt.v1";
pub const VAULT_DECRYPT_SCHEMA: &str = "opensks.vault-decrypt.v1";
pub const VAULT_STATUS_SCHEMA: &str = "opensks.vault-status.v1";
pub const VAULT_ERROR_SCHEMA: &str = "opensks.vault-error.v1";

/// The sanitized, git-trackable summary record written under
/// `.opensks/summaries/`. `contains_raw_transcript` is always `false` and
/// `redacted` is always `true`: this record holds decisions + run links only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultSummary {
    pub schema: String,
    pub conversation_id: String,
    pub title: String,
    pub decisions: Vec<String>,
    pub run_links: Vec<String>,
    pub contains_raw_transcript: bool,
    pub redacted: bool,
    pub generated_at_ms: u64,
}

/// The `vault export-summary` CLI result envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultSummaryResult {
    pub schema: String,
    pub conversation_id: String,
    pub summary_path: String,
    pub decisions: u64,
    pub run_links: Vec<String>,
    pub contains_raw_transcript: bool,
    pub redacted: bool,
}

/// The `vault encrypt` success envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultEncryptResult {
    pub schema: String,
    pub vault_path: String,
    pub recipient: String,
    pub bytes: u64,
}

/// The `vault decrypt` success envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultDecryptResult {
    pub schema: String,
    pub conversation_id: String,
    pub bytes: u64,
}

/// One `.age` vault entry in `vault status`. The recipient is masked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultEntry {
    pub path: String,
    pub recipient_redacted: String,
}

/// One summary entry in `vault status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultSummaryEntry {
    pub path: String,
    pub conversation_id: String,
    pub decisions: u64,
    pub run_links: u64,
}

/// The `vault status` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultStatusResult {
    pub schema: String,
    pub summaries: Vec<VaultSummaryEntry>,
    pub vaults: Vec<VaultEntry>,
}

/// Stable error codes for the `vault encrypt` / `vault decrypt` failure
/// contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VaultErrorCode {
    EncryptFailed,
    BadRecipient,
    DecryptFailed,
}

/// The inner error body for [`VaultErrorEnvelope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultErrorBody {
    pub code: VaultErrorCode,
}

/// The `opensks.vault-error.v1` failure envelope emitted (with a nonzero exit)
/// on any encryption/decryption error. Carries no plaintext.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultErrorEnvelope {
    pub schema: VaultErrorSchemaTag,
    pub error: VaultErrorBody,
}

/// A one-variant tag enum so the error `schema` field serializes to the exact
/// literal `"opensks.vault-error.v1"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum VaultErrorSchemaTag {
    #[serde(rename = "opensks.vault-error.v1")]
    VaultErrorV1,
}

impl VaultErrorEnvelope {
    pub fn new(code: VaultErrorCode) -> Self {
        Self {
            schema: VaultErrorSchemaTag::VaultErrorV1,
            error: VaultErrorBody { code },
        }
    }
}
