# Architecture exception: root `vault` command dispatch (PR-042)

**Date:** 2026-06-22
**PR:** PR-042 — Portable Conversation Summaries and Encrypted Vault
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20160 -> 20168

## What

PR-042 adds a top-level `vault` verb (export-summary / encrypt / decrypt /
status). The command body lives in `crates/opensks-cli`
(`opensks_cli::run_vault_command`), which forwards to the new `opensks-vault`
crate; root `src/lib.rs` only gains the dispatch arm plus a thin wrapper (mirrors
the `file` / `intel` verbs).

## Why this is allowed

ALL crypto + data-plane logic lives in `opensks-vault` (which uses the vetted
`age` crate — X25519 + authenticated ChaCha20-Poly1305 — with NO hand-rolled
primitives, enforced by a self-scanning test) + `opensks-cli`. Root keeps only the
routing shim.
