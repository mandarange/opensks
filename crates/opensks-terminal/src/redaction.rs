use std::path::Path;

use sha2::{Digest, Sha256};

pub fn redact_command(command: &str, workspace: &Path) -> String {
    let command = replace_known_paths(command, workspace);
    redact_sensitive_tokens(&redact_authorization_bearer(&command))
}

pub fn redact_output_for_summary(output: &str, workspace: &Path) -> String {
    let output = strip_ansi_escape_sequences(output);
    let output = replace_known_paths(&output, workspace);
    redact_sensitive_tokens(&redact_authorization_bearer(&output))
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", to_hex(&digest))
}

pub(crate) fn redact_path(path: &Path, workspace: &Path) -> String {
    let raw = path.display().to_string();
    replace_known_paths(&raw, workspace)
}

fn replace_known_paths(value: &str, workspace: &Path) -> String {
    let mut redacted = value.to_string();
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            redacted = redacted.replace(&home, "<home>");
        }
    }
    let workspace = workspace.display().to_string();
    if !workspace.is_empty() {
        redacted = redacted.replace(&workspace, "<workspace>");
    }
    redacted
}

fn redact_authorization_bearer(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for line in value.lines() {
        if let Some(index) = line.to_ascii_lowercase().find("authorization: bearer ") {
            let prefix_len = "authorization: bearer ".len();
            let token_start = index + prefix_len;
            let token_end = line[token_start..]
                .find(|ch: char| ch.is_whitespace() || ch == '"' || ch == '\'')
                .map(|offset| token_start + offset)
                .unwrap_or(line.len());
            result.push_str(&line[..index]);
            result.push_str("Authorization: Bearer <redacted>");
            result.push_str(&line[token_end..]);
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    if !value.ends_with('\n') {
        result.pop();
    }
    result
}

fn redact_sensitive_tokens(value: &str) -> String {
    value
        .split_whitespace()
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_token(token: &str) -> String {
    let trimmed = token.trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == ',');
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains(".env")
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("private_key")
        || lower.contains("private-key")
        || lower.contains("id_rsa")
        || lower.contains("id_ed25519")
        || looks_like_long_hex_or_base64(trimmed)
    {
        return token.replace(trimmed, "<redacted>");
    }
    token.to_string()
}

fn looks_like_long_hex_or_base64(token: &str) -> bool {
    if token.len() < 32 {
        return false;
    }
    let hex = token.chars().all(|ch| ch.is_ascii_hexdigit());
    let base64ish = token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '=' | '_' | '-'));
    hex || base64ish
}

fn strip_ansi_escape_sequences(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_paths_and_long_tokens() {
        let workspace = Path::new("/tmp/workspace");
        let command = "cd /tmp/workspace && cat .env abcdefabcdefabcdefabcdefabcdefabcd";
        let redacted = redact_command(command, workspace);
        assert!(redacted.contains("<workspace>"));
        assert!(redacted.contains("<redacted>"));
    }

    #[test]
    fn digests_are_sha256_labeled() {
        assert!(digest_bytes(b"hello").starts_with("sha256:"));
    }

    #[test]
    fn redacts_bearer_token_without_dropping_line_tail() {
        let workspace = Path::new("/tmp/workspace");
        let command =
            "curl -H \"Authorization: Bearer abc123\" https://api.example.com/endpoint";
        let redacted = redact_command(command, workspace);
        assert!(redacted.contains("https://api.example.com/endpoint"));
        assert!(!redacted.contains("abc123"));
    }
}
