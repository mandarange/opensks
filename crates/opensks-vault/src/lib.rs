//! Portable summaries + encrypted vault (PR-042).
//!
//! Two complementary capabilities for moving a conversation off this machine:
//!
//! 1. A **sanitized summary** (`export_summary`): a small, git-trackable record of
//!    the decisions and run links for a conversation. It is run through the same
//!    secret redaction as the searchable store, it never contains the raw
//!    transcript, and it lives under `.opensks/summaries/` which `.gitignore`
//!    explicitly tracks.
//! 2. An **opt-in encrypted vault** (`encrypt_transcript` / `decrypt_vault`): the
//!    FULL transcript bytes encrypted to an `age` X25519 recipient. Importing the
//!    transcript elsewhere is only possible with the matching `age` identity.
//!
//! # Crypto provenance
//!
//! Every byte of confidentiality here comes from the well-vetted [`age`] crate:
//! X25519 recipient stanzas wrapping a file key, and the age payload format's
//! authenticated ChaCha20-Poly1305. This module contains **no** hand-rolled
//! cipher, KDF, MAC, nonce scheme, or key exchange — see [`encrypt_transcript`]
//! and [`decrypt_vault`] for the exact `age` API calls.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use age::secrecy::ExposeSecret;
use age::x25519::{Identity, Recipient};
use opensks_contracts::{VaultEntry, VaultStatusResult, VaultSummary, VaultSummaryEntry};
use opensks_conversation::ConversationRepository;

pub use opensks_contracts::{
    VAULT_DECRYPT_SCHEMA, VAULT_ENCRYPT_SCHEMA, VAULT_ERROR_SCHEMA, VAULT_STATUS_SCHEMA,
    VAULT_SUMMARY_SCHEMA, VaultDecryptResult, VaultEncryptResult, VaultErrorCode,
    VaultErrorEnvelope,
};

/// Git-trackable, sanitized summaries live here. `.gitignore` tracks this dir.
pub const SUMMARIES_RELATIVE_DIR: &str = ".opensks/summaries";
/// Ciphertext `.age` vaults live here.
pub const VAULTS_RELATIVE_DIR: &str = ".opensks/vaults";

/// Errors surfaced by the vault. Encryption failures deliberately split into
/// `BadRecipient` (the supplied age pubkey did not parse) and `EncryptFailed`
/// (any other failure while producing ciphertext) so the CLI can emit the
/// contract's `bad_recipient` / `encrypt_failed` codes.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("conversation error: {0}")]
    Conversation(#[from] opensks_conversation::ConversationError),
    #[error("artifact io error: {0}")]
    Artifact(#[from] opensks_artifacts::ArtifactError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("conversation not found: {0}")]
    NotFound(String),
    #[error("invalid age recipient")]
    BadRecipient,
    #[error("encryption failed")]
    EncryptFailed,
    #[error("decryption failed")]
    DecryptFailed,
}

/// Stable error code for the CLI/JSON contract.
impl VaultError {
    pub fn code(&self) -> &'static str {
        match self {
            VaultError::BadRecipient => "bad_recipient",
            VaultError::DecryptFailed => "decrypt_failed",
            VaultError::EncryptFailed
            | VaultError::Conversation(_)
            | VaultError::Artifact(_)
            | VaultError::Io(_)
            | VaultError::NotFound(_) => "encrypt_failed",
        }
    }
}

type Result<T> = std::result::Result<T, VaultError>;

/// Result of writing a summary to disk (`summary_path` is what the CLI reports).
///
/// The sanitized, git-trackable [`VaultSummary`] record itself lives in
/// `opensks-contracts` so the Rust and Swift halves share one definition;
/// `contains_raw_transcript` is hard-coded `false` and `redacted` `true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryWrite {
    pub summary: VaultSummary,
    pub summary_path: PathBuf,
}

/// Result of a successful encrypt: where the `.age` landed, the (redacted)
/// recipient, and the ciphertext byte length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptResult {
    pub vault_path: PathBuf,
    pub recipient: String,
    pub bytes: u64,
}

/// Result of a successful decrypt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptResult {
    pub conversation_id: String,
    pub plaintext: Vec<u8>,
    pub bytes: u64,
}

// ---------------------------------------------------------------------------
// 1. Sanitized, git-trackable summary export.
// ---------------------------------------------------------------------------

/// Build a sanitized [`VaultSummary`] from the durable conversation store.
///
/// Decisions are derived from assistant/user message lines that look like
/// decisions, each redacted with [`opensks_conversation::redact_secrets`]; run
/// links come from the conversation's linked runs. No raw transcript is read
/// into the summary — only already-redacted `content_redacted` columns are
/// consulted, and we keep at most a short, scrubbed decision line per message.
pub fn build_summary(
    repo: &ConversationRepository,
    conversation_id: &str,
    now_ms: u64,
) -> Result<VaultSummary> {
    let conversation = repo
        .get_conversation(conversation_id)?
        .ok_or_else(|| VaultError::NotFound(conversation_id.to_string()))?;

    // `content_redacted` is already secret-scrubbed in the store; we redact a
    // second time defensively so the summary can never regress on a secret.
    let messages = repo.message_page(conversation_id, None, usize::MAX)?;
    let mut decisions = Vec::new();
    for message in &messages {
        for line in message.content_redacted.lines() {
            if let Some(decision) = decision_line(line) {
                let redacted = opensks_conversation::redact_secrets(decision);
                let trimmed = redacted.trim();
                if !trimmed.is_empty() {
                    decisions.push(trimmed.to_string());
                }
            }
        }
    }

    let run_links = repo
        .runs_for_conversation(conversation_id)?
        .into_iter()
        .map(|run| run.run_id)
        .collect();

    Ok(VaultSummary {
        schema: VAULT_SUMMARY_SCHEMA.to_string(),
        conversation_id: conversation_id.to_string(),
        title: opensks_conversation::redact_secrets(&conversation.title),
        decisions,
        run_links,
        // INVARIANT: a summary never carries the raw transcript and is redacted.
        contains_raw_transcript: false,
        redacted: true,
        generated_at_ms: now_ms,
    })
}

/// Heuristic: treat a line as a "decision" when it begins with a decision
/// marker. Returns the cleaned decision text (without the marker), or `None`.
fn decision_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    const MARKERS: [&str; 6] = [
        "decision:",
        "decided:",
        "- decision",
        "* decision",
        "decision -",
        "we decided",
    ];
    let lower = trimmed.to_ascii_lowercase();
    if MARKERS.iter().any(|m| lower.starts_with(m)) {
        // Strip everything up to and including the first ':' when present so the
        // stored decision is the substance, not the marker.
        let body = trimmed
            .split_once(':')
            .map(|(_, rest)| rest)
            .unwrap_or(trimmed);
        let body = body.trim();
        if body.is_empty() {
            Some(trimmed)
        } else {
            Some(body)
        }
    } else {
        None
    }
}

/// Write the sanitized summary to a git-trackable path under
/// `.opensks/summaries/<conversation_id>.summary.json` using an atomic write.
pub fn export_summary(
    repo: &ConversationRepository,
    workspace: &Path,
    conversation_id: &str,
    now_ms: u64,
) -> Result<SummaryWrite> {
    let summary = build_summary(repo, conversation_id, now_ms)?;
    let summary_path = summary_path_for(workspace, conversation_id);
    let json = serde_json::to_string_pretty(&summary)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    opensks_artifacts::write_text_atomic(&summary_path, &(json + "\n"))?;
    Ok(SummaryWrite {
        summary,
        summary_path,
    })
}

/// The canonical summary path for a conversation.
pub fn summary_path_for(workspace: &Path, conversation_id: &str) -> PathBuf {
    workspace
        .join(SUMMARIES_RELATIVE_DIR)
        .join(format!("{conversation_id}.summary.json"))
}

/// The canonical `.age` vault path for a conversation.
pub fn vault_path_for(workspace: &Path, conversation_id: &str) -> PathBuf {
    workspace
        .join(VAULTS_RELATIVE_DIR)
        .join(format!("{conversation_id}.age"))
}

// ---------------------------------------------------------------------------
// 2/3. age-encrypted vault (opt-in).
// ---------------------------------------------------------------------------

/// Reconstruct the FULL transcript bytes for a conversation as a UTF-8 JSON
/// document of ordered messages. This is the *plaintext* that gets encrypted —
/// it is never written to disk in the clear by this crate.
pub fn transcript_bytes(repo: &ConversationRepository, conversation_id: &str) -> Result<Vec<u8>> {
    let conversation = repo
        .get_conversation(conversation_id)?
        .ok_or_else(|| VaultError::NotFound(conversation_id.to_string()))?;
    let messages = repo.message_page(conversation_id, None, usize::MAX)?;
    let doc = serde_json::json!({
        "schema": "opensks.vault-transcript.v1",
        "conversation_id": conversation_id,
        "title": conversation.title,
        "messages": messages,
    });
    let bytes = serde_json::to_vec_pretty(&doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(bytes)
}

/// Parse an age X25519 recipient public key (`age1...`). A parse failure maps
/// to [`VaultError::BadRecipient`] so the CLI can emit `bad_recipient`.
pub fn parse_recipient(recipient: &str) -> Result<Recipient> {
    Recipient::from_str(recipient.trim()).map_err(|_| VaultError::BadRecipient)
}

/// Encrypt arbitrary plaintext bytes to an age X25519 recipient, fully in
/// memory. Uses [`age::Encryptor::with_recipients`] (X25519 stanza wrapping a
/// random file key) and the age format's authenticated ChaCha20-Poly1305
/// payload. No custom crypto. Returns the ciphertext bytes on success; on any
/// failure returns [`VaultError::EncryptFailed`] and produces no output.
pub fn encrypt_bytes(plaintext: &[u8], recipient: &Recipient) -> Result<Vec<u8>> {
    let recipients = [recipient as &dyn age::Recipient];
    let encryptor = age::Encryptor::with_recipients(recipients.into_iter())
        .map_err(|_| VaultError::EncryptFailed)?;
    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|_| VaultError::EncryptFailed)?;
    writer
        .write_all(plaintext)
        .map_err(|_| VaultError::EncryptFailed)?;
    writer.finish().map_err(|_| VaultError::EncryptFailed)?;
    Ok(ciphertext)
}

/// Encrypt a conversation's full transcript to a `.age` vault.
///
/// **Atomic + fail-closed**: the ciphertext is produced entirely in memory, then
/// written to a sibling temp file, fsync'd, and only `rename`d to the final
/// `.age` path on success. The plaintext transcript is never written to disk. On
/// any error nothing is left at `vault_path` and no temp file remains.
pub fn encrypt_transcript(
    repo: &ConversationRepository,
    workspace: &Path,
    conversation_id: &str,
    recipient_str: &str,
) -> Result<EncryptResult> {
    // Parse the recipient FIRST so a bad key fails before we read anything.
    let recipient = parse_recipient(recipient_str)?;
    let plaintext = transcript_bytes(repo, conversation_id)?;
    let ciphertext = encrypt_bytes(&plaintext, &recipient)?;

    let vault_path = vault_path_for(workspace, conversation_id);
    // `write_text_atomic` works on UTF-8; age armor is optional, our ciphertext
    // is binary, so we do the atomic temp+fsync+rename ourselves on bytes.
    write_bytes_atomic(&vault_path, &ciphertext)?;

    Ok(EncryptResult {
        vault_path,
        recipient: recipient_str.trim().to_string(),
        bytes: ciphertext.len() as u64,
    })
}

/// Atomic binary write: temp sibling → fsync → rename. On any failure the temp
/// file is removed so no partial output survives. Never writes plaintext.
fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "vault path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("age.{}.tmp", std::process::id()));
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if let Err(error) = write {
        // Fail-closed: leave nothing behind.
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(path);
        return Err(VaultError::Io(error));
    }
    Ok(())
}

/// Decrypt a `.age` vault using one or more age identities. A wrong/missing
/// identity maps to [`VaultError::DecryptFailed`] and leaks no plaintext.
pub fn decrypt_bytes(ciphertext: &[u8], identities: &[Identity]) -> Result<Vec<u8>> {
    let decryptor = age::Decryptor::new(ciphertext).map_err(|_| VaultError::DecryptFailed)?;
    let mut reader = decryptor
        .decrypt(identities.iter().map(|i| i as &dyn age::Identity))
        .map_err(|_| VaultError::DecryptFailed)?;
    let mut plaintext = Vec::new();
    reader
        .read_to_end(&mut plaintext)
        .map_err(|_| VaultError::DecryptFailed)?;
    Ok(plaintext)
}

/// Parse age X25519 identities from text. Each non-comment, non-empty line that
/// parses as an `AGE-SECRET-KEY-1...` identity is used.
pub fn parse_identities(contents: &str) -> Result<Vec<Identity>> {
    let mut identities = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Ok(identity) = Identity::from_str(line) {
            identities.push(identity);
        }
    }
    if identities.is_empty() {
        return Err(VaultError::DecryptFailed);
    }
    Ok(identities)
}

/// Decrypt ciphertext with identity text from a secure store. The identity text
/// is never written by this crate; callers own secret-store provenance.
pub fn decrypt_bytes_with_identity_text(ciphertext: &[u8], identity_text: &str) -> Result<Vec<u8>> {
    let identities = parse_identities(identity_text)?;
    decrypt_bytes(ciphertext, &identities)
}

/// Encrypt plaintext with the public recipient derived from the first identity
/// in secure-store text. This lets a workspace raw-prompt vault use one stored
/// age identity for both producer-side encryption and supervisor-side decrypt.
pub fn encrypt_bytes_with_identity_text(plaintext: &[u8], identity_text: &str) -> Result<Vec<u8>> {
    let identities = parse_identities(identity_text)?;
    let recipient = identities
        .first()
        .ok_or(VaultError::EncryptFailed)?
        .to_public();
    encrypt_bytes(plaintext, &recipient)
}

/// Generate a fresh age X25519 identity in the text format accepted by
/// [`parse_identities`]. Callers must immediately place the returned secret text
/// in a secure store and must never log or persist it elsewhere.
pub fn generate_identity_text() -> String {
    let identity = Identity::generate();
    let secret = identity.to_string();
    format!(
        "# OpenSKS raw prompt vault identity\n{}\n",
        secret.expose_secret()
    )
}

/// Load age X25519 identities from an identity file.
pub fn load_identities(identity_file: &Path) -> Result<Vec<Identity>> {
    let contents = std::fs::read_to_string(identity_file)?;
    parse_identities(&contents)
}

/// Decrypt a `.age` vault file with an identity file and recover the transcript.
pub fn decrypt_vault(vault_path: &Path, identity_file: &Path) -> Result<DecryptResult> {
    let ciphertext = std::fs::read(vault_path).map_err(|_| VaultError::DecryptFailed)?;
    let identities = load_identities(identity_file)?;
    let plaintext = decrypt_bytes(&ciphertext, &identities)?;
    let conversation_id = serde_json::from_slice::<serde_json::Value>(&plaintext)
        .ok()
        .and_then(|v| {
            v.get("conversation_id")
                .and_then(|c| c.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let bytes = plaintext.len() as u64;
    Ok(DecryptResult {
        conversation_id,
        plaintext,
        bytes,
    })
}

// ---------------------------------------------------------------------------
// 4. status
// ---------------------------------------------------------------------------

/// Redact an age recipient for display: keep the `age1` prefix and a short tail,
/// mask the middle so the full pubkey is not echoed into status output.
pub fn redact_recipient(recipient: &str) -> String {
    let recipient = recipient.trim();
    if let Some(rest) = recipient.strip_prefix("age1") {
        if rest.len() > 6 {
            let tail = &rest[rest.len() - 4..];
            return format!("age1…{tail}");
        }
    }
    "age1…".to_string()
}

/// Scan the workspace for sanitized summaries and `.age` vaults.
pub fn status(workspace: &Path) -> Result<VaultStatusResult> {
    let mut summaries = Vec::new();
    let summaries_dir = workspace.join(SUMMARIES_RELATIVE_DIR);
    if summaries_dir.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&summaries_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        entries.sort();
        for path in entries {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(summary) = serde_json::from_str::<VaultSummary>(&text) {
                    summaries.push(VaultSummaryEntry {
                        path: relative_display(workspace, &path),
                        conversation_id: summary.conversation_id,
                        decisions: summary.decisions.len() as u64,
                        run_links: summary.run_links.len() as u64,
                    });
                }
            }
        }
    }

    let mut vaults = Vec::new();
    let vaults_dir = workspace.join(VAULTS_RELATIVE_DIR);
    if vaults_dir.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&vaults_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "age"))
            .collect();
        entries.sort();
        for path in entries {
            // The recipient is inside the ciphertext header; we do not (and
            // cannot, without a key) recover it, so status reports it masked.
            vaults.push(VaultEntry {
                path: relative_display(workspace, &path),
                recipient_redacted: "age1…".to_string(),
            });
        }
    }

    Ok(VaultStatusResult {
        schema: VAULT_STATUS_SCHEMA.to_string(),
        summaries,
        vaults,
    })
}

fn relative_display(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::secrecy::ExposeSecret;
    use opensks_conversation::ConversationRepository;

    /// A throwaway workspace directory that cleans itself up.
    struct TempWorkspace {
        path: PathBuf,
    }

    impl TempWorkspace {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "opensks-vault-test-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("create temp workspace");
            Self { path }
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Build a workspace-backed repo with a conversation seeded with the given
    /// (role, text) messages and `run_id`s linked. Returns the conversation id.
    fn seed_conversation(
        workspace: &Path,
        messages: &[(opensks_contracts::MessageRole, &str)],
        run_ids: &[&str],
    ) -> (ConversationRepository, String) {
        let repo = ConversationRepository::open_workspace(workspace).expect("open repo");
        let project = repo
            .create_project("ws-key", "Workspace", 1)
            .expect("project");
        let conversation = repo
            .create_conversation(&project, "Design discussion", 2)
            .expect("conversation");
        for (i, (role, text)) in messages.iter().enumerate() {
            let turn = format!("turn-{i}");
            let message_id = repo
                .append_message(
                    &project,
                    &conversation,
                    &turn,
                    *role,
                    opensks_contracts::MessageState::Complete,
                    text,
                    (10 + i) as u64,
                )
                .expect("append");
            if let Some(run_id) = run_ids.get(i) {
                repo.link_run(
                    &conversation,
                    &message_id,
                    &turn,
                    run_id,
                    "primary",
                    (10 + i) as u64,
                )
                .expect("link run");
            }
        }
        (repo, conversation)
    }

    /// Write an age identity to a file the way the CLI's `--identity-file`
    /// expects, returning the path.
    fn write_identity_file(dir: &Path, name: &str, identity: &Identity) -> PathBuf {
        let path = dir.join(name);
        let secret = identity.to_string();
        std::fs::write(
            &path,
            format!("# age identity\n{}\n", secret.expose_secret()),
        )
        .expect("write identity");
        path
    }

    #[test]
    fn export_summary_is_redacted_and_has_no_raw_transcript() {
        let ws = TempWorkspace::new("summary");
        let secret = "redaction-test-secret-fixture-0003";
        let (repo, conversation) = seed_conversation(
            &ws.path,
            &[
                (
                    opensks_contracts::MessageRole::User,
                    "Decision: adopt the age crate for vault encryption",
                ),
                (
                    opensks_contracts::MessageRole::Assistant,
                    &format!("Here is the api key {secret} do not share it"),
                ),
                (
                    opensks_contracts::MessageRole::Assistant,
                    "We decided to keep summaries git-trackable",
                ),
            ],
            &["run-0001", "run-0002", "run-0003"],
        );

        let written = export_summary(&repo, &ws.path, &conversation, 99).expect("export");

        // contains_raw_transcript MUST be false and redacted true.
        assert!(!written.summary.contains_raw_transcript);
        assert!(written.summary.redacted);

        // Decisions reconstructed from the decision-marked lines.
        assert!(
            written
                .summary
                .decisions
                .iter()
                .any(|d| d.contains("adopt the age crate")),
            "decisions: {:?}",
            written.summary.decisions
        );
        assert!(
            written
                .summary
                .decisions
                .iter()
                .any(|d| d.contains("keep summaries git-trackable")),
            "decisions: {:?}",
            written.summary.decisions
        );

        // Run links reconstructed.
        assert_eq!(
            written.summary.run_links,
            vec![
                "run-0001".to_string(),
                "run-0002".to_string(),
                "run-0003".to_string()
            ]
        );

        // The on-disk file contains NO secret and NO raw assistant body.
        let on_disk = std::fs::read_to_string(&written.summary_path).expect("read summary");
        assert!(
            !on_disk.contains(secret),
            "summary leaked a secret: {on_disk}"
        );
        // The raw "do not share it" assistant transcript line is not a decision
        // and must not appear in the summary at all.
        assert!(
            !on_disk.contains("do not share it"),
            "summary leaked raw transcript: {on_disk}"
        );
        assert!(on_disk.contains("\"contains_raw_transcript\": false"));
    }

    #[test]
    fn encrypt_roundtrips_only_via_decrypt_with_matching_identity() {
        let ws = TempWorkspace::new("roundtrip");
        let (repo, conversation) = seed_conversation(
            &ws.path,
            &[(
                opensks_contracts::MessageRole::User,
                "the full transcript body lives only in the encrypted vault",
            )],
            &["run-aaaa"],
        );

        let identity = Identity::generate();
        let recipient = identity.to_public().to_string();

        let original = transcript_bytes(&repo, &conversation).expect("transcript");
        let result =
            encrypt_transcript(&repo, &ws.path, &conversation, &recipient).expect("encrypt");

        assert!(result.vault_path.exists());
        assert_eq!(result.recipient, recipient);
        assert!(result.bytes > 0);

        // The ciphertext on disk is NOT the plaintext.
        let ciphertext = std::fs::read(&result.vault_path).expect("read vault");
        assert_ne!(ciphertext, original);
        assert!(
            !ciphertext
                .windows(b"full transcript body".len())
                .any(|w| w == b"full transcript body"),
            "plaintext leaked into ciphertext"
        );
        // age binary format begins with the "age-encryption.org" header.
        assert!(ciphertext.starts_with(b"age-encryption.org/v1"));

        // Round-trip ONLY via decrypt with the matching identity.
        let identity_file = write_identity_file(&ws.path, "id.txt", &identity);
        let decrypted = decrypt_vault(&result.vault_path, &identity_file).expect("decrypt");
        assert_eq!(decrypted.plaintext, original);
        assert_eq!(decrypted.conversation_id, conversation);
        assert_eq!(decrypted.bytes, original.len() as u64);
    }

    #[test]
    fn wrong_identity_fails_to_decrypt_and_leaks_no_plaintext() {
        let ws = TempWorkspace::new("wrongkey");
        let (repo, conversation) = seed_conversation(
            &ws.path,
            &[(
                opensks_contracts::MessageRole::User,
                "secret-transcript-only-for-the-right-key",
            )],
            &[],
        );

        let right = Identity::generate();
        let recipient = right.to_public().to_string();
        let result =
            encrypt_transcript(&repo, &ws.path, &conversation, &recipient).expect("encrypt");

        // A DIFFERENT identity cannot decrypt.
        let wrong = Identity::generate();
        let wrong_file = write_identity_file(&ws.path, "wrong.txt", &wrong);
        let err = decrypt_vault(&result.vault_path, &wrong_file).expect_err("must fail");
        assert_eq!(err.code(), "decrypt_failed");
    }

    #[test]
    fn bad_recipient_writes_no_age_and_no_plaintext() {
        let ws = TempWorkspace::new("badrecipient");
        let (repo, conversation) = seed_conversation(
            &ws.path,
            &[(
                opensks_contracts::MessageRole::User,
                "plaintext that must never touch disk on a bad recipient",
            )],
            &[],
        );

        let vault_path = vault_path_for(&ws.path, &conversation);
        let err = encrypt_transcript(&repo, &ws.path, &conversation, "not-an-age-recipient")
            .expect_err("bad recipient must fail");
        assert_eq!(err.code(), "bad_recipient");

        // NO .age vault file exists.
        assert!(
            !vault_path.exists(),
            "a .age file was left behind on bad recipient"
        );
        // No temp file lingers next to it either.
        if let Some(parent) = vault_path.parent() {
            if parent.is_dir() {
                let leftovers: Vec<_> = std::fs::read_dir(parent)
                    .unwrap()
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .collect();
                assert!(
                    leftovers.is_empty(),
                    "stray files left in vault dir: {leftovers:?}"
                );
            }
        }

        // The plaintext transcript must not have been written by the VAULT: the
        // conversation message itself legitimately lives in the source-of-record
        // SQLite store under `.opensks/runtime/` (git-ignored). What must NOT
        // exist is any vault output (a `.age` file or temp) carrying it. Scope
        // the plaintext scan to the vault dir, which the encrypt path owns.
        let needle = b"plaintext that must never touch disk";
        if let Some(vault_dir) = vault_path.parent() {
            assert!(
                !workspace_contains_bytes(vault_dir, needle),
                "plaintext transcript was written into the vault dir on a bad recipient"
            );
        }
        // Sanity: the only place the message text exists is the conversation
        // store, never a `.age` vault.
        assert!(
            workspace_contains_bytes(&ws.path.join(".opensks/runtime"), needle),
            "expected the source message to remain in the conversation store"
        );
    }

    /// Recursively confirm no file under `root` contains `needle`.
    fn workspace_contains_bytes(root: &Path, needle: &[u8]) -> bool {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    if bytes.windows(needle.len()).any(|w| w == needle) {
                        return true;
                    }
                }
            }
        }
        false
    }

    #[test]
    fn status_lists_summaries_and_vaults() {
        let ws = TempWorkspace::new("status");
        let (repo, conversation) = seed_conversation(
            &ws.path,
            &[(
                opensks_contracts::MessageRole::User,
                "Decision: ship vault status",
            )],
            &["run-zzzz"],
        );
        export_summary(&repo, &ws.path, &conversation, 1).expect("export");
        let identity = Identity::generate();
        encrypt_transcript(
            &repo,
            &ws.path,
            &conversation,
            &identity.to_public().to_string(),
        )
        .expect("encrypt");

        let status = status(&ws.path).expect("status");
        assert_eq!(status.schema, VAULT_STATUS_SCHEMA);
        assert_eq!(status.summaries.len(), 1);
        assert_eq!(status.summaries[0].conversation_id, conversation);
        assert_eq!(status.vaults.len(), 1);
        // The recipient is masked in status output.
        assert!(status.vaults[0].recipient_redacted.starts_with("age1"));
        assert!(
            !status.vaults[0].recipient_redacted.contains(
                identity
                    .to_public()
                    .to_string()
                    .strip_prefix("age1")
                    .unwrap()
            )
        );
    }

    /// Provenance guard: the only crypto in this crate comes from the `age`
    /// crate. There must be no hand-rolled cipher/KDF/MAC/nonce/key-exchange.
    #[test]
    fn source_uses_age_and_no_hand_rolled_crypto() {
        let full = include_str!("lib.rs");
        // Scan only the PRODUCTION code: everything before the test module. This
        // excludes this guard's own forbidden-token list literals.
        let source = full.split("#[cfg(test)]").next().unwrap_or(full);
        // We DO drive the age crate for all confidentiality.
        assert!(source.contains("age::Encryptor::with_recipients"));
        assert!(source.contains("age::Decryptor::new"));

        // Scan only executable code (strip `//` comment lines and blank-out the
        // tail of inline comments) so the crypto-provenance prose in our doc
        // comments does not false-positive. The point of the guard is that no
        // low-level primitive is *invoked or imported* — never hand-rolled.
        let code: String = source
            .lines()
            .map(|line| {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    ""
                } else if let Some((before, _)) = line.split_once("//") {
                    before
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Low-level crypto crates / primitive symbols that would indicate a
        // hand-rolled cipher, KDF, MAC, nonce scheme, or key exchange.
        for forbidden in [
            "chacha20",
            "chacha20poly1305",
            "poly1305",
            "aes_gcm",
            "aes::",
            "hmac::",
            "sha2::",
            "x25519_dalek",
            "curve25519_dalek",
            "scrypt::scrypt",
            "ring::",
            "fn encrypt_block",
            "fn xor_keystream",
        ] {
            assert!(
                !code.to_ascii_lowercase().contains(forbidden),
                "vault code references low-level primitive `{forbidden}`; \
                 all crypto must go through the age crate"
            );
        }
    }

    #[test]
    fn smoke_age_roundtrip() {
        let identity = Identity::generate();
        let recipient = identity.to_public();
        let ciphertext = encrypt_bytes(b"hello vault", &recipient).expect("encrypt");
        let plaintext = decrypt_bytes(&ciphertext, &[identity]).expect("decrypt");
        assert_eq!(plaintext, b"hello vault");
    }

    #[test]
    fn identity_text_encrypt_decrypt_roundtrips() {
        let identity = Identity::generate();
        let secret = identity.to_string();
        let identity_text = format!(
            "# workspace raw prompt vault identity\n{}\n",
            secret.expose_secret()
        );
        let ciphertext =
            encrypt_bytes_with_identity_text(b"raw prompt bytes", &identity_text).expect("encrypt");
        assert!(!String::from_utf8_lossy(&ciphertext).contains("raw prompt bytes"));
        let plaintext =
            decrypt_bytes_with_identity_text(&ciphertext, &identity_text).expect("decrypt");
        assert_eq!(plaintext, b"raw prompt bytes");
    }

    /// Git-trackability policy (PR-042): a raw transcript path IS ignored while a
    /// sanitized summary path is NOT. Runs `git check-ignore` against the real
    /// repository `.gitignore`. `git check-ignore -q` exits 0 when the path is
    /// ignored and 1 when it is not.
    #[test]
    fn gitignore_tracks_summaries_and_ignores_raw_transcripts() {
        // Walk up from this crate to the workspace root (the dir holding `.git`).
        let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        while !root.join(".git").exists() {
            if !root.pop() {
                eprintln!("skipping: no .git found above CARGO_MANIFEST_DIR");
                return;
            }
        }

        let check_ignored = |relative: &str| -> Option<bool> {
            let output = std::process::Command::new("git")
                .arg("check-ignore")
                .arg("-q")
                .arg(relative)
                .current_dir(&root)
                .status();
            match output {
                Ok(status) => match status.code() {
                    Some(0) => Some(true),  // ignored
                    Some(1) => Some(false), // not ignored (tracked)
                    _ => None,              // git error / unavailable
                },
                Err(_) => None,
            }
        };

        // Raw transcript stores must be ignored.
        for ignored in [
            ".opensks/transcripts/raw.json",
            ".opensks/vaults/conv-1.age",
            "scratch.plaintext",
            ".opensks/runtime/conversations.sqlite3",
        ] {
            if let Some(is_ignored) = check_ignored(ignored) {
                assert!(is_ignored, "expected `{ignored}` to be git-ignored");
            }
        }

        // Sanitized summaries must be tracked (NOT ignored).
        if let Some(is_ignored) = check_ignored(".opensks/summaries/conv-1.summary.json") {
            assert!(
                !is_ignored,
                "expected sanitized summary to be git-trackable (not ignored)"
            );
        }
    }
}
